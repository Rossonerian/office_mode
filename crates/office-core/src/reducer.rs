use crate::alerts::Detector;
use crate::event::{Event, EventKind};
use crate::model::{Agent, AgentId, AgentKind, AgentState, Alert, Scene};
use std::collections::BTreeMap;

/// Walk-in animation length: Arriving -> Idle.
pub const ARRIVE_MS: u64 = 2_000;
/// Walk-out animation length: Leaving -> removed.
pub const LEAVE_MS: u64 = 2_000;
/// Agents with no events for this long (and not Idle/NeedsYou) start Leaving.
pub const STALE_MS: u64 = 10 * 60 * 1_000;
/// Max alerts kept in Scene.alerts (oldest dropped first; newest last).
pub const MAX_ALERTS: usize = 200;

/// Tool name -> character state (shared by every source).
pub fn state_for_tool(tool: &str) -> AgentState {
    match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => AgentState::Typing,
        "Read" | "Grep" | "Glob" | "WebFetch" | "WebSearch" | "LS" => AgentState::Reading,
        "Bash" | "BashOutput" | "Monitor" => AgentState::Running,
        "Agent" | "Task" => AgentState::Delegating,
        "AskUserQuestion" => AgentState::NeedsYou,
        _ => AgentState::Running,
    }
}

/// Infer kind/parent for an agent first seen through a non-AgentSeen event.
fn infer_kind(id: &str) -> (AgentKind, Option<AgentId>) {
    if let Some(rest) = id.strip_prefix("cc-sub:") {
        let parent = rest.split('/').next().map(|s| format!("cc:{s}"));
        (AgentKind::ClaudeSub, parent)
    } else if id.starts_with("cc:") {
        (AgentKind::Claude, None)
    } else if id.starts_with("agy:") {
        (AgentKind::Agy, None)
    } else {
        (AgentKind::Other, None)
    }
}

fn new_agent(id: &str, kind: AgentKind, parent: Option<AgentId>, at: u64) -> Agent {
    Agent {
        id: id.to_string(),
        kind,
        label: id.to_string(),
        parent,
        cwd: None,
        role: None,
        model: None,
        slot: None,
        pid: None,
        state: AgentState::Arriving,
        activity: String::new(),
        started_ms: at,
        last_event_ms: at,
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        tokens_cache_write: 0,
        cost_usd: 0.0,
        context_pct: None,
        rss_mb: None,
        tool_calls: 0,
        tool_failures: 0,
        resume_id: None,
        pending_permission: None,
    }
}

/// Folds Events into the Scene. Signatures are part of the contract; bodies are owned by workstream A.
#[derive(Default)]
pub struct Reducer {
    scene: Scene,
    detector: Detector,
    /// When each Leaving agent started leaving.
    leaving_since: BTreeMap<AgentId, u64>,
    /// Tokens/cost of agents already removed, so totals never go backwards.
    retired_tokens: u64,
    retired_cost: f64,
}

impl Reducer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reducer whose alert thresholds come from `cfg` (cost_alert_usd, stuck_after_s).
    pub fn with_config(cfg: &crate::config::Config) -> Self {
        Reducer { detector: Detector::new(cfg), ..Self::default() }
    }

    fn push_alerts(&mut self, alerts: Vec<Alert>) {
        if alerts.is_empty() {
            return;
        }
        self.scene.alerts.extend(alerts);
        let n = self.scene.alerts.len();
        if n > MAX_ALERTS {
            self.scene.alerts.drain(..n - MAX_ALERTS);
        }
    }

    /// Clear all alerts (TUI `c`).
    pub fn clear_alerts(&mut self) {
        self.scene.alerts.clear();
    }

    fn recompute_totals(&mut self) {
        let (mut t, mut c) = (self.retired_tokens, self.retired_cost);
        for a in self.scene.agents.values() {
            t += a.tokens_shown();
            c += a.cost_usd;
        }
        self.scene.total_tokens = t;
        self.scene.total_cost_usd = c;
    }

    /// Get or create (state Arriving) an agent for events that reference an unknown id.
    fn ensure(&mut self, id: &str, at: u64) -> &mut Agent {
        if !self.scene.agents.contains_key(id) {
            let (kind, parent) = infer_kind(id);
            self.scene.agents.insert(id.to_string(), new_agent(id, kind, parent, at));
        }
        let a = self.scene.agents.get_mut(id).expect("just inserted");
        a.last_event_ms = a.last_event_ms.max(at);
        a
    }

    fn set_state(&mut self, id: &str, state: AgentState, at: u64) {
        if let Some(a) = self.scene.agents.get_mut(id) {
            a.state = state;
            if state == AgentState::Leaving {
                self.leaving_since.entry(id.to_string()).or_insert(at);
            } else {
                self.leaving_since.remove(id);
            }
        }
    }

    /// Apply one event. Returns true if the scene changed.
    pub fn apply(&mut self, ev: Event) -> bool {
        let at = ev.at_ms;
        self.scene.now_ms = self.scene.now_ms.max(at);
        let alerts = self.detector.observe(&ev, &self.scene);
        let had_alerts = !alerts.is_empty();
        self.push_alerts(alerts);

        let changed = match ev.kind {
            EventKind::AgentSeen { id, kind, label, parent, cwd, role, model, slot, pid, resume_id } => {
                let exists = self.scene.agents.contains_key(&id);
                if !exists {
                    self.scene.agents.insert(id.clone(), new_agent(&id, kind, None, at));
                }
                let revive = self.scene.agents.get(&id).map(|a| a.state == AgentState::Leaving).unwrap_or(false);
                let a = self.scene.agents.get_mut(&id).expect("present");
                a.kind = kind;
                a.last_event_ms = a.last_event_ms.max(at);
                if let Some(v) = label {
                    a.label = v;
                }
                if parent.is_some() {
                    a.parent = parent;
                }
                if cwd.is_some() {
                    a.cwd = cwd;
                }
                if role.is_some() {
                    a.role = role;
                }
                if model.is_some() {
                    a.model = model;
                }
                if slot.is_some() {
                    a.slot = slot;
                }
                if pid.is_some() {
                    a.pid = pid;
                }
                if resume_id.is_some() {
                    a.resume_id = resume_id;
                }
                if revive {
                    self.set_state(&id, AgentState::Idle, at);
                }
                true
            }
            EventKind::State { id, state, activity } => {
                let a = self.ensure(&id, at);
                if let Some(act) = activity {
                    a.activity = act;
                }
                if matches!(state, AgentState::Idle | AgentState::Done | AgentState::Leaving) {
                    a.pending_permission = None;
                }
                self.set_state(&id, state, at);
                true
            }
            EventKind::ToolStart { id, tool, summary } => {
                let a = self.ensure(&id, at);
                a.tool_calls += 1;
                a.activity = if summary.is_empty() { tool.clone() } else { format!("{tool} {summary}") };
                self.set_state(&id, state_for_tool(&tool), at);
                true
            }
            EventKind::ToolEnd { id, tool: _, ok } => {
                let a = self.ensure(&id, at);
                if !ok {
                    a.tool_failures += 1;
                }
                a.pending_permission = None;
                self.set_state(&id, AgentState::Thinking, at);
                true
            }
            EventKind::Usage { id, tokens_in, tokens_out, tokens_cache_read, tokens_cache_write, cost_usd, context_pct } => {
                // Usage carries running totals (not deltas).
                let a = self.ensure(&id, at);
                a.tokens_in = tokens_in;
                a.tokens_out = tokens_out;
                a.tokens_cache_read = tokens_cache_read;
                a.tokens_cache_write = tokens_cache_write;
                a.cost_usd = cost_usd;
                if context_pct.is_some() {
                    a.context_pct = context_pct;
                }
                true
            }
            EventKind::Resources { id, rss_mb } => match self.scene.agents.get_mut(&id) {
                Some(a) => {
                    a.last_event_ms = a.last_event_ms.max(at);
                    let ch = a.rss_mb != Some(rss_mb);
                    a.rss_mb = Some(rss_mb);
                    ch
                }
                None => false,
            },
            EventKind::PermissionRequested { id, request_id, summary } => {
                let a = self.ensure(&id, at);
                a.pending_permission = Some(request_id);
                a.activity = summary;
                self.set_state(&id, AgentState::NeedsYou, at);
                true
            }
            EventKind::PermissionResolved { id, request_id } => match self.scene.agents.get_mut(&id) {
                Some(a) if a.pending_permission.as_deref() == Some(request_id.as_str()) => {
                    a.pending_permission = None;
                    a.last_event_ms = a.last_event_ms.max(at);
                    if a.state == AgentState::NeedsYou {
                        self.set_state(&id, AgentState::Thinking, at);
                    }
                    true
                }
                _ => false,
            },
            EventKind::Gone { id } => {
                if let Some(a) = self.scene.agents.get_mut(&id) {
                    a.pending_permission = None;
                    a.last_event_ms = a.last_event_ms.max(at);
                    self.set_state(&id, AgentState::Leaving, at);
                    true
                } else {
                    false
                }
            }
            EventKind::Router(status) => {
                self.scene.router = status;
                true
            }
            EventKind::Alert(_) => false, // already passed through by the Detector
        };
        if changed {
            self.recompute_totals();
        }
        changed || had_alerts
    }

    /// Periodic housekeeping: Arriving->Idle after walk-in, stale/Gone agents -> Leaving -> removed,
    /// stuck detection hand-off to alerts. Called ~every 500 ms.
    pub fn sweep(&mut self, now_ms: u64) -> bool {
        self.scene.now_ms = self.scene.now_ms.max(now_ms);
        let mut changed = false;
        let mut to_idle = Vec::new();
        let mut to_leave = Vec::new();
        let mut to_remove = Vec::new();
        for (id, a) in &self.scene.agents {
            match a.state {
                AgentState::Arriving if now_ms.saturating_sub(a.started_ms) >= ARRIVE_MS => to_idle.push(id.clone()),
                AgentState::Leaving => {
                    let since = self.leaving_since.get(id).copied().unwrap_or(now_ms);
                    if now_ms.saturating_sub(since) >= LEAVE_MS {
                        to_remove.push(id.clone());
                    } else if !self.leaving_since.contains_key(id) {
                        to_leave.push(id.clone()); // Leaving without a timestamp: start the clock now
                    }
                }
                AgentState::Idle | AgentState::NeedsYou => {}
                _ if now_ms.saturating_sub(a.last_event_ms) >= STALE_MS => to_leave.push(id.clone()),
                _ => {}
            }
        }
        for id in to_idle {
            self.set_state(&id, AgentState::Idle, now_ms);
            changed = true;
        }
        for id in to_leave {
            if let Some(a) = self.scene.agents.get_mut(&id) {
                a.state = AgentState::Leaving;
            }
            self.leaving_since.insert(id, now_ms);
            changed = true;
        }
        for id in to_remove {
            if let Some(a) = self.scene.agents.remove(&id) {
                self.retired_tokens += a.tokens_shown();
                self.retired_cost += a.cost_usd;
            }
            self.leaving_since.remove(&id);
            changed = true;
        }
        let alerts = self.detector.tick(&self.scene, now_ms);
        if !alerts.is_empty() {
            self.push_alerts(alerts);
            changed = true;
        }
        if changed {
            self.recompute_totals();
        }
        changed
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SourceKind;
    use crate::model::{AlertLevel, RouterStatus};

    fn ev(at: u64, kind: EventKind) -> Event {
        Event { at_ms: at, source: SourceKind::Demo, kind }
    }
    fn seen(id: &str, label: Option<&str>, cwd: Option<&str>) -> EventKind {
        EventKind::AgentSeen {
            id: id.into(),
            kind: AgentKind::Claude,
            label: label.map(Into::into),
            parent: None,
            cwd: cwd.map(Into::into),
            role: None,
            model: Some("opus".into()),
            slot: None,
            pid: None,
            resume_id: None,
        }
    }
    fn codes(r: &Reducer) -> Vec<String> {
        r.scene().alerts.iter().map(|a| a.code.clone()).collect()
    }

    #[test]
    fn agent_seen_upsert_keeps_existing_fields() {
        let mut r = Reducer::new();
        assert!(r.apply(ev(1, seen("cc:a", Some("cc·DICE"), Some("/x/DICE")))));
        r.apply(ev(2, seen("cc:a", None, None)));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.label, "cc·DICE");
        assert_eq!(a.cwd.as_deref(), Some("/x/DICE"));
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!(a.state, AgentState::Arriving);
        assert_eq!(a.started_ms, 1);
        assert_eq!(a.last_event_ms, 2);
    }

    #[test]
    fn arriving_to_idle_after_2s() {
        let mut r = Reducer::new();
        r.apply(ev(1000, seen("cc:a", None, None)));
        assert!(!r.sweep(2500));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Arriving);
        assert!(r.sweep(3000));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Idle);
    }

    #[test]
    fn tool_states_and_counters() {
        let mut r = Reducer::new();
        r.apply(ev(1, seen("cc:a", None, None)));
        r.apply(ev(2, EventKind::ToolStart { id: "cc:a".into(), tool: "Edit".into(), summary: "main.rs".into() }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::Typing);
        assert_eq!(a.activity, "Edit main.rs");
        r.apply(ev(3, EventKind::ToolEnd { id: "cc:a".into(), tool: "Edit".into(), ok: false }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::Thinking);
        assert_eq!((a.tool_calls, a.tool_failures), (1, 1));
        for (t, s) in [("Grep", AgentState::Reading), ("Bash", AgentState::Running), ("Task", AgentState::Delegating)] {
            r.apply(ev(4, EventKind::ToolStart { id: "cc:a".into(), tool: t.into(), summary: String::new() }));
            assert_eq!(r.scene().agents["cc:a"].state, s);
        }
    }

    #[test]
    fn unknown_sub_is_created_with_parent() {
        let mut r = Reducer::new();
        r.apply(ev(1, EventKind::State { id: "cc-sub:s1/ag1".into(), state: AgentState::Thinking, activity: None }));
        let a = &r.scene().agents["cc-sub:s1/ag1"];
        assert_eq!(a.kind, AgentKind::ClaudeSub);
        assert_eq!(a.parent.as_deref(), Some("cc:s1"));
    }

    #[test]
    fn usage_totals_survive_removal() {
        let mut r = Reducer::new();
        r.apply(ev(1, seen("cc:a", None, None)));
        r.apply(ev(1, seen("cc:b", None, None)));
        let u = |id: &str, i, o, c| EventKind::Usage {
            id: id.into(),
            tokens_in: i,
            tokens_out: o,
            tokens_cache_read: 1_000_000,
            tokens_cache_write: 0,
            cost_usd: c,
            context_pct: Some(10.0),
        };
        r.apply(ev(2, u("cc:a", 100, 50, 1.0)));
        r.apply(ev(3, u("cc:a", 200, 60, 2.0))); // running totals, not deltas
        r.apply(ev(3, u("cc:b", 10, 0, 0.5)));
        assert_eq!(r.scene().total_tokens, 270);
        assert!((r.scene().total_cost_usd - 2.5).abs() < 1e-9);
        r.apply(ev(4, EventKind::Gone { id: "cc:a".into() }));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Leaving);
        r.sweep(5000);
        assert!(r.sweep(6100) || !r.scene().agents.contains_key("cc:a"));
        assert!(!r.scene().agents.contains_key("cc:a"));
        assert_eq!(r.scene().total_tokens, 270);
        assert!((r.scene().total_cost_usd - 2.5).abs() < 1e-9);
    }

    #[test]
    fn gone_then_removed_after_2s() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(10_000, EventKind::Gone { id: "cc:a".into() }));
        r.sweep(11_000);
        assert!(r.scene().agents.contains_key("cc:a"));
        r.sweep(12_000);
        assert!(!r.scene().agents.contains_key("cc:a"));
    }

    #[test]
    fn stale_agents_leave_but_idle_and_needs_you_stay() {
        let mut r = Reducer::new();
        for id in ["cc:busy", "cc:idle", "cc:ask"] {
            r.apply(ev(0, seen(id, None, None)));
        }
        r.apply(ev(1, EventKind::State { id: "cc:busy".into(), state: AgentState::Running, activity: None }));
        r.apply(ev(1, EventKind::State { id: "cc:idle".into(), state: AgentState::Idle, activity: None }));
        r.apply(ev(1, EventKind::State { id: "cc:ask".into(), state: AgentState::NeedsYou, activity: None }));
        let t = STALE_MS + 10;
        r.sweep(t);
        assert_eq!(r.scene().agents["cc:busy"].state, AgentState::Leaving);
        assert_eq!(r.scene().agents["cc:idle"].state, AgentState::Idle);
        assert_eq!(r.scene().agents["cc:ask"].state, AgentState::NeedsYou);
        r.sweep(t + LEAVE_MS);
        assert!(!r.scene().agents.contains_key("cc:busy"));
    }

    #[test]
    fn permission_flow() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::PermissionRequested { id: "cc:a".into(), request_id: "r1".into(), summary: "Bash rm".into() }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::NeedsYou);
        assert_eq!(a.pending_permission.as_deref(), Some("r1"));
        assert!(codes(&r).contains(&"needs_you".to_string()));
        r.apply(ev(2, EventKind::PermissionResolved { id: "cc:a".into(), request_id: "other".into() }));
        assert!(r.scene().agents["cc:a"].pending_permission.is_some());
        r.apply(ev(3, EventKind::PermissionResolved { id: "cc:a".into(), request_id: "r1".into() }));
        let a = &r.scene().agents["cc:a"];
        assert!(a.pending_permission.is_none());
        assert_eq!(a.state, AgentState::Thinking);
    }

    #[test]
    fn loop_alert_after_four_repeats() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        for i in 0..5 {
            r.apply(ev(i, EventKind::ToolStart { id: "cc:a".into(), tool: "Bash".into(), summary: "npm test".into() }));
        }
        assert_eq!(codes(&r).iter().filter(|c| *c == "loop").count(), 1);
    }

    #[test]
    fn failure_streak_alert() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        let end = |ok| EventKind::ToolEnd { id: "cc:a".into(), tool: "Bash".into(), ok };
        r.apply(ev(1, end(false)));
        r.apply(ev(2, end(false)));
        r.apply(ev(3, end(true)));
        r.apply(ev(4, end(false)));
        r.apply(ev(5, end(false)));
        assert!(codes(&r).is_empty());
        r.apply(ev(6, end(false)));
        assert_eq!(codes(&r), vec!["tool_failures"]);
    }

    #[test]
    fn stuck_alert_once_per_quiet_period() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1000, EventKind::State { id: "cc:a".into(), state: AgentState::Running, activity: None }));
        r.sweep(100_000);
        assert!(codes(&r).is_empty());
        r.sweep(122_000);
        r.sweep(125_000);
        assert_eq!(codes(&r), vec!["stuck"]);
        // new activity re-arms it
        r.apply(ev(130_000, EventKind::ToolStart { id: "cc:a".into(), tool: "Bash".into(), summary: "x".into() }));
        r.sweep(260_000);
        assert_eq!(codes(&r), vec!["stuck", "stuck"]);
    }

    #[test]
    fn cost_alert_fires_at_each_multiple() {
        let mut r = Reducer::new(); // default cost_alert_usd = 50
        r.apply(ev(0, seen("cc:a", None, None)));
        for c in [1.0, 5.5, 49.9, 50.5, 70.0, 99.0, 100.0, 160.0, 120.0, 161.0] {
            r.apply(ev(1, EventKind::Usage {
                id: "cc:a".into(),
                tokens_in: 1,
                tokens_out: 1,
                tokens_cache_read: 0,
                tokens_cache_write: 0,
                cost_usd: c,
                context_pct: None,
            }));
        }
        // $50, $100, $150 (the jump to 160 crosses one more multiple); no repeats
        assert_eq!(codes(&r), vec!["cost", "cost", "cost"]);
        assert_eq!(r.scene().alerts[0].level, AlertLevel::Critical);
        assert!(r.scene().alerts[2].message.contains("$150.00"), "{}", r.scene().alerts[2].message);
    }

    #[test]
    fn usage_without_cache_fields_still_parses() {
        // events/agents serialised before the cache split default the cache fields to 0
        let ev: EventKind = serde_json::from_str(
            r#"{"Usage":{"id":"cc:a","tokens_in":5,"tokens_out":2,"cost_usd":0.1,"context_pct":null}}"#,
        )
        .unwrap();
        assert!(matches!(ev, EventKind::Usage { tokens_cache_read: 0, tokens_cache_write: 0, .. }));
        let mut a = serde_json::to_value(new_agent("cc:a", AgentKind::Claude, None, 0)).unwrap();
        a.as_object_mut().unwrap().remove("tokens_cache_read");
        a.as_object_mut().unwrap().remove("tokens_cache_write");
        let a: crate::model::Agent = serde_json::from_value(a).unwrap();
        assert_eq!((a.tokens_cache_read, a.tokens_cache_write), (0, 0));
    }

    #[test]
    fn router_alerts_pass_through_and_cap() {
        let mut r = Reducer::new();
        r.apply(ev(0, EventKind::Router(RouterStatus { capacity: Some(3), ..Default::default() })));
        assert_eq!(r.scene().router.capacity, Some(3));
        for i in 0..(MAX_ALERTS as u64 + 50) {
            let a = Alert { at_ms: i, level: AlertLevel::Warn, agent: None, code: "timeout".into(), message: format!("m{i}") };
            assert!(r.apply(ev(i, EventKind::Alert(a))));
        }
        let al = &r.scene().alerts;
        assert_eq!(al.len(), MAX_ALERTS);
        assert_eq!(al.last().unwrap().message, format!("m{}", MAX_ALERTS + 49));
        assert_eq!(al[0].message, "m50");
    }

    #[test]
    fn needs_you_state_alert_on_transition_only() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        let s = || EventKind::State { id: "cc:a".into(), state: AgentState::NeedsYou, activity: Some("permission".into()) };
        r.apply(ev(1, s()));
        r.apply(ev(2, s()));
        assert_eq!(codes(&r), vec!["needs_you"]);
    }

    #[test]
    fn agent_seen_revives_leaving_agent() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::Gone { id: "cc:a".into() }));
        r.apply(ev(2, seen("cc:a", None, None)));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Idle);
        r.sweep(10_000);
        assert!(r.scene().agents.contains_key("cc:a"));
    }
}
