//! `office demo`: a synthetic, deterministic 60 s storyline that exercises every AgentState,
//! a permission request, router BUSY / tool_denied alerts, a loop, router status updates,
//! and agents arriving and leaving. Needs no real sources.
use office_core::{
    model::AgentKind, Alert, AlertLevel, AgentState, Event, EventKind, RouterStatus, SourceKind,
};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub const CYCLE_S: u64 = 60;

pub const DICE: &str = "cc:demo-dice";
pub const OFFICE: &str = "cc:demo-office";

/// Ids of the transient agents for a given cycle (they leave and come back as new arrivals).
pub struct Cast {
    pub explore: String,
    pub plan: String,
    pub general: String,
    pub arch: String,
    pub worker: String,
    pub tester: String,
    pub pids: [u32; 3],
}

pub fn cast(cycle: u64) -> Cast {
    let base = 91_000 + (cycle % 500) as u32 * 10;
    let pids = [base + 1, base + 2, base + 3];
    Cast {
        explore: format!("cc-sub:demo-dice/explore-{cycle}"),
        plan: format!("cc-sub:demo-dice/plan-{cycle}"),
        general: format!("cc-sub:demo-office/general-{cycle}"),
        arch: format!("agy:{}", pids[0]),
        worker: format!("agy:{}", pids[1]),
        tester: format!("agy:{}", pids[2]),
        pids,
    }
}

#[allow(clippy::too_many_arguments)]
fn seen(
    id: &str,
    kind: AgentKind,
    label: &str,
    parent: Option<&str>,
    cwd: &str,
    role: Option<&str>,
    model: &str,
    slot: Option<u32>,
    pid: Option<u32>,
    resume: Option<&str>,
) -> EventKind {
    EventKind::AgentSeen {
        id: id.into(),
        kind,
        label: Some(label.into()),
        parent: parent.map(Into::into),
        cwd: Some(cwd.into()),
        role: role.map(Into::into),
        model: Some(model.into()),
        slot,
        pid,
        resume_id: resume.map(Into::into),
    }
}

fn st(id: &str, state: AgentState, act: &str) -> EventKind {
    EventKind::State { id: id.into(), state, activity: Some(act.into()) }
}

fn tool(id: &str, tool: &str, summary: &str) -> EventKind {
    EventKind::ToolStart { id: id.into(), tool: tool.into(), summary: summary.into() }
}

fn alert(t_ms: u64, level: AlertLevel, agent: Option<&str>, code: &str, msg: &str) -> EventKind {
    EventKind::Alert(Alert { at_ms: t_ms, level, agent: agent.map(Into::into), code: code.into(), message: msg.into() })
}

fn router(capacity: u32, busy: u32, free: u32, pressure: u8, queued: u32, limit: Option<&str>) -> EventKind {
    EventKind::Router(RouterStatus {
        capacity: Some(capacity),
        max_concurrent: Some(5),
        busy_slots: busy,
        free_mb: Some(free),
        per_proc_mb: Some(280),
        pressure: Some(pressure),
        queued,
        limit_text: limit.map(Into::into),
    })
}

/// Running usage totals for an agent present since `from` (cumulative, as the reducer expects).
fn usage(id: &str, secs: u64, in_rate: u64, out_rate: u64, price_in: f64, price_out: f64) -> EventKind {
    let tin = 4_000 + secs * in_rate;
    let tout = secs * out_rate;
    EventKind::Usage {
        id: id.into(),
        tokens_in: tin,
        tokens_out: tout,
        // long sessions re-read their context from the prompt cache every turn
        tokens_cache_read: secs * in_rate * 6,
        tokens_cache_write: 2_000 + secs * in_rate / 5,
        cost_usd: (tin as f64 * price_in + tout as f64 * price_out) / 1_000_000.0,
        context_pct: Some(((tin as f32) / 2_000.0).min(95.0)),
        model: None,
    }
}

/// Events for second `t` (0..60) of cycle `cycle`. `home` is used for display cwds.
pub fn script(cycle: u64, t: u64, home: &str, now_ms: u64) -> Vec<EventKind> {
    let c = cast(cycle);
    let dice_cwd = format!("{home}/Projects/Apps/DICE");
    let office_cwd = format!("{home}/Projects/Tools/agent-office");
    let (dc, oc) = (dice_cwd.as_str(), office_cwd.as_str());
    let mut v = Vec::new();
    let agy = |id: &str, role: &str, model: &str, slot: Option<u32>, pid: u32| {
        let short = model.trim_start_matches("gemini-4-");
        seen(id, AgentKind::Agy, &format!("{role}·{short}"), Some(OFFICE), oc, Some(role), model, slot, Some(pid), None)
    };
    let sub = |id: &str, role: &str, parent: &str, cwd: &str| {
        seen(id, AgentKind::ClaudeSub, role, Some(parent), cwd, Some(role), "claude-haiku-4-5", None, None, None)
    };
    match t {
        0 => {
            v.push(seen(DICE, AgentKind::Claude, "cc·DICE", None, dc, None, "claude-opus-4-5", None, None, Some("demo-dice-session")));
            v.push(seen(OFFICE, AgentKind::Claude, "cc·agent-office", None, oc, None, "claude-sonnet-4-5", None, None, Some("demo-office-session")));
            v.push(st(DICE, AgentState::Idle, "waiting for a prompt"));
            v.push(st(OFFICE, AgentState::Idle, "waiting for a prompt"));
            v.push(router(3, 0, 2400, 1, 0, None));
        }
        1 => v.push(st(DICE, AgentState::Thinking, "Planning the refactor")),
        3 => v.push(tool(DICE, "Read", "src/main.rs")),
        4 => v.push(tool(OFFICE, "Edit", "crates/office/src/app/mod.rs")),
        5 => {
            v.push(sub(&c.explore, "Explore", DICE, dc));
            v.push(st(&c.explore, AgentState::Arriving, "walking in"));
            v.push(st(DICE, AgentState::Delegating, "waiting on 2 subagents"));
        }
        6 => {
            v.push(sub(&c.plan, "Plan", DICE, dc));
            v.push(st(&c.plan, AgentState::Arriving, "walking in"));
        }
        7 => {
            v.push(tool(&c.explore, "Read", "docs/DESIGN.md"));
            v.push(st(&c.plan, AgentState::Thinking, "Sketching the steps"));
        }
        8 => {
            v.push(agy(&c.arch, "council-architect", "gemini-4-neon-xhigh", Some(0), c.pids[0]));
            v.push(st(&c.arch, AgentState::Thinking, "Drafting the proposal"));
            v.push(router(3, 1, 2100, 1, 0, None));
        }
        10 => v.push(tool(OFFICE, "Bash", "cargo test -p office")),
        12 => {
            v.push(agy(&c.worker, "worker", "gemini-4-hydrogen-low", Some(1), c.pids[1]));
            v.push(st(&c.worker, AgentState::Typing, "Edit crates/office/src/app/ui.rs"));
            v.push(router(3, 2, 1800, 1, 0, None));
        }
        14 => {
            v.push(agy(&c.tester, "tester", "gemini-4-hydrogen-high", None, c.pids[2]));
            v.push(st(&c.tester, AgentState::Queued, "waiting for a router slot"));
            v.push(router(2, 2, 1100, 2, 1, Some("capacity 2 reached (memory pressure warn); tester queued")));
            v.push(alert(now_ms, AlertLevel::Warn, Some(&c.tester), "router_busy", "agy-route BUSY: tester queued (2/2 slots)"));
        }
        16 => {
            v.push(sub(&c.general, "general-purpose", OFFICE, oc));
            v.push(tool(&c.general, "Bash", "npm test"));
        }
        18 => v.push(EventKind::PermissionRequested {
            id: OFFICE.into(),
            request_id: format!("demo-perm-{cycle}"),
            summary: "Bash: rm -rf target/debug/incremental".into(),
        }),
        // Same tool + same summary 4x in a row: the loop detector fires.
        20..=23 => v.push(tool(&c.explore, "Grep", "TODO")),
        24 => v.push(st(&c.arch, AgentState::Done, "Proposal ready")),
        25 => {
            v.push(st(&c.arch, AgentState::Leaving, "heading out"));
            v.push(router(3, 1, 1900, 1, 1, None));
        }
        26 => {
            v.push(EventKind::Gone { id: c.arch.clone() });
            v.push(agy(&c.tester, "tester", "gemini-4-hydrogen-high", Some(0), c.pids[2]));
            v.push(tool(&c.tester, "Bash", "pytest -q"));
            v.push(router(3, 2, 1700, 1, 0, None));
            v.push(st(&c.plan, AgentState::Done, "Plan written"));
        }
        27 => v.push(st(&c.plan, AgentState::Leaving, "heading out")),
        28 => {
            v.push(EventKind::Gone { id: c.plan.clone() });
            v.push(EventKind::ToolEnd { id: c.worker.clone(), tool: "Edit".into(), ok: false });
            v.push(st(&c.worker, AgentState::Error, "TOOL_DENIED: write outside the repo"));
            v.push(alert(now_ms, AlertLevel::Critical, Some(&c.worker), "tool_denied", "agy worker: TOOL_DENIED (write outside repo)"));
        }
        30 => {
            // Auto-resolve if nobody answered the permission prompt.
            v.push(EventKind::PermissionResolved { id: OFFICE.into(), request_id: format!("demo-perm-{cycle}") });
            v.push(tool(OFFICE, "Bash", "rm -rf target/debug/incremental"));
        }
        32 => v.push(st(&c.worker, AgentState::Typing, "Retry: edit app/ui.rs")),
        34 => v.push(tool(&c.tester, "Read", "tests/test_ws.py")),
        35 => v.push(st(&c.explore, AgentState::Done, "Found 12 TODOs")),
        36 => v.push(st(&c.explore, AgentState::Leaving, "heading out")),
        37 => {
            v.push(EventKind::Gone { id: c.explore.clone() });
            v.push(tool(DICE, "Edit", "src/lib.rs"));
        }
        40 => v.push(st(&c.general, AgentState::Done, "tests passed")),
        41 => v.push(st(&c.general, AgentState::Leaving, "heading out")),
        42 => v.push(EventKind::Gone { id: c.general.clone() }),
        43 => v.push(st(DICE, AgentState::Idle, "waiting for a prompt")),
        44 => v.push(st(&c.worker, AgentState::Done, "Edits applied")),
        45 => {
            v.push(st(&c.worker, AgentState::Leaving, "heading out"));
            v.push(router(3, 1, 2000, 1, 0, None));
        }
        46 => v.push(EventKind::Gone { id: c.worker.clone() }),
        48 => v.push(st(&c.tester, AgentState::Done, "PASS")),
        49 => v.push(st(&c.tester, AgentState::Leaving, "heading out")),
        50 => {
            v.push(EventKind::Gone { id: c.tester.clone() });
            v.push(router(3, 0, 2400, 1, 0, None));
            v.push(st(OFFICE, AgentState::Thinking, "Summarising the results"));
        }
        52 => {
            v.push(st(DICE, AgentState::Error, "API overloaded (529), retrying"));
            v.push(alert(now_ms, AlertLevel::Warn, Some(OFFICE), "cost", "cc·agent-office passed the $5.00 budget"));
        }
        54 => v.push(st(DICE, AgentState::Idle, "waiting for a prompt")),
        56 => v.push(st(OFFICE, AgentState::Idle, "waiting for a prompt")),
        _ => {}
    }

    // Steady background: usage totals and RSS for whoever is present.
    let total = cycle * CYCLE_S + t;
    v.push(usage(DICE, total, 400, 60, 15.0, 75.0));
    v.push(usage(OFFICE, total, 250, 40, 3.0, 15.0));
    for (id, from, to, rate) in [
        (&c.explore, 5, 36, 300),
        (&c.plan, 6, 27, 200),
        (&c.general, 16, 41, 150),
        (&c.arch, 8, 25, 500),
        (&c.worker, 12, 45, 350),
        (&c.tester, 14, 49, 250),
    ] {
        if t > from && t <= to {
            v.push(usage(id, t - from, rate, rate / 6, 1.0, 4.0));
            if id.starts_with("agy:") {
                v.push(EventKind::Resources { id: id.clone(), rss_mb: 240 + ((t * 7) % 40) as u32 });
            }
        }
    }
    v
}

/// Start the demo thread; it stops when the receiver goes away.
pub fn spawn(tx: Sender<Event>) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into());
    let _ = std::thread::Builder::new().name("office-demo".into()).spawn(move || {
        let mut cycle = 0u64;
        loop {
            for t in 0..CYCLE_S {
                let now = office_core::paths::now_ms();
                for k in script(cycle, t, &home, now) {
                    if tx.send(Event { at_ms: now, source: SourceKind::Demo, kind: k }).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            cycle += 1;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::ALL_STATES;
    use office_core::{reducer::state_for_tool, Reducer};
    use std::collections::HashSet;

    fn states_in(evs: &[EventKind]) -> Vec<AgentState> {
        evs.iter()
            .filter_map(|e| match e {
                EventKind::State { state, .. } => Some(*state),
                EventKind::ToolStart { tool, .. } => Some(state_for_tool(tool)),
                EventKind::PermissionRequested { .. } => Some(AgentState::NeedsYou),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn script_covers_every_state_and_feature() {
        let all: Vec<EventKind> = (0..CYCLE_S).flat_map(|t| script(0, t, "/h", t * 1000)).collect();
        let states: HashSet<_> = states_in(&all).into_iter().map(|s| format!("{s:?}")).collect();
        for s in ALL_STATES {
            assert!(states.contains(&format!("{s:?}")), "demo never shows {s:?}");
        }
        let codes: HashSet<String> =
            all.iter().filter_map(|e| if let EventKind::Alert(a) = e { Some(a.code.clone()) } else { None }).collect();
        assert!(codes.contains("router_busy") && codes.contains("tool_denied"));
        assert!(all.iter().any(|e| matches!(e, EventKind::PermissionRequested { .. })));
        assert!(all.iter().filter(|e| matches!(e, EventKind::Router(_))).count() >= 5);
        assert_eq!(all.iter().filter(|e| matches!(e, EventKind::Gone { .. })).count(), 6);
        let agy_roles: HashSet<String> = all
            .iter()
            .filter_map(|e| match e {
                EventKind::AgentSeen { kind: AgentKind::Agy, role, .. } => role.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(agy_roles, ["council-architect", "worker", "tester"].into_iter().map(String::from).collect());
        // Cycles use fresh transient ids.
        assert_ne!(cast(0).worker, cast(1).worker);
    }

    /// Drive two cycles through the real reducer (workstream A) and check the scene evolves.
    #[test]
    fn demo_through_reducer() {
        let mut r = Reducer::new();
        let mut max_agents = 0;
        let mut seen_states = HashSet::new();
        let mut needs_you_with_perm = false;
        for cycle in 0..2u64 {
            for t in 0..CYCLE_S {
                let now = (cycle * CYCLE_S + t) * 1000 + 1;
                for k in script(cycle, t, "/h", now) {
                    r.apply(Event { at_ms: now, source: SourceKind::Demo, kind: k });
                }
                r.sweep(now + 500);
                let s = r.scene();
                max_agents = max_agents.max(s.agents.len());
                for a in s.agents.values() {
                    seen_states.insert(format!("{:?}", a.state));
                    if a.state == AgentState::NeedsYou && a.pending_permission.is_some() {
                        needs_you_with_perm = true;
                    }
                }
            }
        }
        let s = r.scene();
        assert_eq!(max_agents, 8);
        assert!(needs_you_with_perm);
        assert!(seen_states.len() >= 10, "{seen_states:?}");
        let codes: HashSet<_> = s.alerts.iter().map(|a| a.code.as_str()).collect();
        assert!(codes.contains("loop"), "{codes:?}");
        assert!(codes.contains("router_busy") && codes.contains("tool_denied"));
        // At the end of a cycle only the two Claude sessions remain.
        assert_eq!(s.agents.len(), 2, "{:?}", s.agents.keys().collect::<Vec<_>>());
        assert!(s.total_tokens > 0 && s.total_cost_usd > 0.0);
    }
}
