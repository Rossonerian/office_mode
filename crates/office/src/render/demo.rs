//! A synthetic scene for previews/tests: 2 Claude pods + an agy lab, covering every AgentState.
use office_core::{model::AgentKind, Agent, AgentState, Scene};

fn mk(id: &str, kind: AgentKind, label: &str, parent: Option<&str>, cwd: &str, state: AgentState, activity: &str, started: u64, last: u64) -> Agent {
    Agent {
        id: id.into(),
        kind,
        label: label.into(),
        parent: parent.map(Into::into),
        cwd: Some(cwd.into()),
        role: None,
        model: None,
        slot: None,
        pid: None,
        state,
        activity: activity.into(),
        started_ms: started,
        last_event_ms: last,
        tokens_in: 12_000,
        tokens_out: 3_400,
        cost_usd: 0.42,
        context_pct: Some(31.0),
        rss_mb: None,
        tool_calls: 7,
        tool_failures: 0,
        resume_id: None,
        pending_permission: None,
    }
}

/// Demo scene at `now` (ms). Arriving/Leaving agents are mid-walk; the Done agent has just finished.
pub fn demo_scene(now: u64) -> Scene {
    use AgentKind::*;
    use AgentState::*;
    let old = now.saturating_sub(600_000);
    let a = [
        mk("cc:alpha", Claude, "cc·DICE", None, "/p/DICE", Delegating, "Task: audit hooks", old, now - 2_000),
        mk("cc-sub:alpha/1", ClaudeSub, "Explore", Some("cc:alpha"), "/p/DICE", Reading, "Read swarm.py", old + 10, now - 500),
        mk("cc-sub:alpha/2", ClaudeSub, "Plan", Some("cc:alpha"), "/p/DICE", Thinking, "", old + 20, now - 800),
        mk("cc-sub:alpha/3", ClaudeSub, "general", Some("cc:alpha"), "/p/DICE", Done, "Finished", old + 30, now - 400),
        mk("cc:beta", Claude, "cc·office", None, "/p/office", NeedsYou, "Allow Bash rm?", old + 40, now - 5_000),
        mk("cc-sub:beta/1", ClaudeSub, "tester", Some("cc:beta"), "/p/office", Running, "cargo test", old + 50, now - 300),
        mk("agy:4101", Agy, "worker·hyd-low", None, "/p/office", Typing, "Edit layout.rs", old + 60, now - 100),
        mk("cc-sub:beta/2", ClaudeSub, "reviewer", Some("cc:beta"), "/p/office", Arriving, "", now - 900, now - 900),
        mk("agy:4102", Agy, "architect·neon", None, "/p/elsewhere", Idle, "", old + 70, now - 9_000),
        mk("agy:4103", Agy, "supervisor·neon", None, "/p/elsewhere", Queued, "waiting for slot", old + 80, now - 1_000),
        mk("other:1", Other, "lint-bot", None, "/tmp", Error, "TIMEOUT", old + 90, now - 700),
        mk("agy:4104", Agy, "tester·hyd-hi", None, "/p/elsewhere", Leaving, "", old + 100, now - 1_200),
    ];
    let mut s = Scene { now_ms: now, ..Default::default() };
    for ag in a {
        s.agents.insert(ag.id.clone(), ag);
    }
    s
}
