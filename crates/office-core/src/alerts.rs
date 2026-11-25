use crate::event::{Event, EventKind};
use crate::model::{AgentId, AgentState, Alert, AlertLevel, Scene};
use std::collections::BTreeMap;

/// Same tool + same summary this many times in a row => "loop".
pub const LOOP_REPEATS: u32 = 4;
/// Failed tool calls in a row => "tool_failures".
pub const FAILURE_STREAK: u32 = 3;

#[derive(Default)]
struct PerAgent {
    last_tool: Option<(String, String)>,
    repeats: u32,
    failures: u32,
    /// last_event_ms at which a stuck alert was raised (re-arms on new activity).
    stuck_alerted_at: Option<u64>,
}

/// Stateful detectors (loop, stuck, failure streak, needs_you, cost). Owned by workstream A.
pub struct Detector {
    cost_alert_usd: f64,
    stuck_after_ms: u64,
    per: BTreeMap<AgentId, PerAgent>,
    /// Highest multiple of `cost_alert_usd` already alerted per agent (re-fires at each multiple).
    cost_alerted: BTreeMap<AgentId, u64>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new(&crate::config::Config::default())
    }
}

fn alert(at_ms: u64, level: AlertLevel, agent: Option<&str>, code: &str, message: String) -> Alert {
    Alert { at_ms, level, agent: agent.map(str::to_string), code: code.into(), message }
}

fn label_of<'a>(scene: &'a Scene, id: &'a str) -> &'a str {
    scene.agents.get(id).map(|a| a.label.as_str()).unwrap_or(id)
}

fn is_needs_you(scene: &Scene, id: &str) -> bool {
    scene.agents.get(id).map(|a| a.state == AgentState::NeedsYou).unwrap_or(false)
}

impl Detector {
    pub fn new(cfg: &crate::config::Config) -> Self {
        Detector {
            cost_alert_usd: cfg.cost_alert_usd,
            stuck_after_ms: cfg.stuck_after_s.max(1) * 1000,
            per: BTreeMap::new(),
            cost_alerted: BTreeMap::new(),
        }
    }

    /// Inspect an event before it's applied; return any alerts it triggers.
    pub fn observe(&mut self, ev: &Event, scene: &Scene) -> Vec<Alert> {
        let at = ev.at_ms;
        let mut out = Vec::new();
        match &ev.kind {
            EventKind::Alert(a) => out.push(a.clone()),
            EventKind::ToolStart { id, tool, summary } => {
                let p = self.per.entry(id.clone()).or_default();
                p.stuck_alerted_at = None;
                let key = (tool.clone(), summary.clone());
                if p.last_tool.as_ref() == Some(&key) {
                    p.repeats += 1;
                } else {
                    p.last_tool = Some(key);
                    p.repeats = 1;
                }
                if p.repeats == LOOP_REPEATS {
                    out.push(alert(
                        at,
                        AlertLevel::Warn,
                        Some(id),
                        "loop",
                        format!("{} repeated {tool} {summary} {LOOP_REPEATS}x", label_of(scene, id)),
                    ));
                }
                if tool == "AskUserQuestion" && !is_needs_you(scene, id) {
                    out.push(alert(at, AlertLevel::Warn, Some(id), "needs_you", format!("{} asks a question", label_of(scene, id))));
                }
            }
            EventKind::ToolEnd { id, tool, ok } => {
                let p = self.per.entry(id.clone()).or_default();
                p.stuck_alerted_at = None;
                if *ok {
                    p.failures = 0;
                } else {
                    p.failures += 1;
                    if p.failures == FAILURE_STREAK {
                        out.push(alert(
                            at,
                            AlertLevel::Warn,
                            Some(id),
                            "tool_failures",
                            format!("{}: {FAILURE_STREAK} failed tool calls in a row (last: {tool})", label_of(scene, id)),
                        ));
                    }
                }
            }
            EventKind::State { id, state, activity } => {
                if let Some(p) = self.per.get_mut(id) {
                    p.stuck_alerted_at = None;
                }
                if *state == AgentState::NeedsYou && !is_needs_you(scene, id) {
                    let what = activity.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "needs your input".into());
                    out.push(alert(at, AlertLevel::Warn, Some(id), "needs_you", format!("{}: {what}", label_of(scene, id))));
                }
            }
            EventKind::PermissionRequested { id, summary, .. } => {
                if !is_needs_you(scene, id) {
                    out.push(alert(
                        at,
                        AlertLevel::Warn,
                        Some(id),
                        "needs_you",
                        format!("{} asks permission: {summary}", label_of(scene, id)),
                    ));
                }
            }
            EventKind::Usage { id, cost_usd, .. } => {
                // fire once per multiple crossed: $50, $100, $150, …
                let step = self.cost_alert_usd;
                let level = if step > 0.0 && cost_usd.is_finite() { (*cost_usd / step).floor() as u64 } else { 0 };
                let done = self.cost_alerted.get(id).copied().unwrap_or(0);
                if level > done {
                    self.cost_alerted.insert(id.clone(), level);
                    out.push(alert(
                        at,
                        AlertLevel::Critical,
                        Some(id),
                        "cost",
                        format!("{} passed ${:.2} (now ${cost_usd:.2})", label_of(scene, id), step * level as f64),
                    ));
                }
            }
            EventKind::Gone { id } => {
                self.per.remove(id);
            }
            _ => {}
        }
        out
    }

    /// Time-based checks (stuck). Called from Reducer::sweep.
    pub fn tick(&mut self, scene: &Scene, now_ms: u64) -> Vec<Alert> {
        let mut out = Vec::new();
        self.per.retain(|id, _| scene.agents.contains_key(id));
        for (id, a) in &scene.agents {
            let watched = !matches!(
                a.state,
                AgentState::Idle
                    | AgentState::Done
                    | AgentState::NeedsYou
                    | AgentState::Queued
                    | AgentState::Leaving
                    | AgentState::Arriving
                    | AgentState::Error
            );
            if !watched
                || now_ms.saturating_sub(a.last_event_ms) < self.stuck_after_ms
                || crate::reducer::has_live_active_children(scene, id)
            {
                continue;
            }
            let p = self.per.entry(id.clone()).or_default();
            if p.stuck_alerted_at == Some(a.last_event_ms) {
                continue;
            }
            p.stuck_alerted_at = Some(a.last_event_ms);
            let secs = now_ms.saturating_sub(a.last_event_ms) / 1000;
            out.push(alert(
                now_ms,
                AlertLevel::Warn,
                Some(id),
                "stuck",
                format!("{}: no activity for {secs}s while {:?}", a.label, a.state),
            ));
        }
        out
    }
}
