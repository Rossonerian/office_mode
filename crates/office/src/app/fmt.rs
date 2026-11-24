//! Pure formatting helpers for the sidebar, header and popups (unit-tested).
use office_core::{model::AgentKind, Agent, AgentState, AlertLevel, Scene};
use ratatui::style::Color;

/// Truncate to at most `max` display chars, ending with '…' when cut.
pub fn trunc(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Compact elapsed time (≤ 4 chars for the table): 42s, 12m, 3h, 2d.
pub fn fmt_elapsed_short(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Longer elapsed time for popups: 42s, 3m05s, 1h02m.
pub fn fmt_elapsed_long(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Relative time for the alerts feed: "now", "12s", "3m", "2h".
pub fn fmt_ago(now_ms: u64, at_ms: u64) -> String {
    let d = now_ms.saturating_sub(at_ms);
    if d < 1000 {
        "now".into()
    } else {
        fmt_elapsed_short(d)
    }
}

/// Tokens in thousands: 0, 950, 12.3k, 123k, 1.2M.
pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        format!("{n}")
    } else if n < 100_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Cost without the $ sign, ≤ 4 chars where possible: 0.42, 12.3, 123.
pub fn fmt_cost_short(usd: f64) -> String {
    if usd < 10.0 {
        format!("{usd:.2}")
    } else if usd < 100.0 {
        format!("{usd:.1}")
    } else {
        format!("{usd:.0}")
    }
}

pub fn fmt_cost(usd: f64) -> String {
    format!("${usd:.2}")
}

/// Strip vendor prefixes: "claude-opus-4-5" -> "opus-4-5", "gemini-4-neon-high" -> "neon-high".
pub fn model_short(model: &str) -> String {
    let m = model.trim();
    for p in ["claude-", "gemini-4-", "gemini-"] {
        if let Some(rest) = m.strip_prefix(p) {
            return rest.to_string();
        }
    }
    m.to_string()
}

/// "role" for agy/subagents, else the short model name.
pub fn role_or_model(a: &Agent) -> String {
    match (&a.role, &a.model) {
        (Some(r), _) if !r.is_empty() => r.clone(),
        (_, Some(m)) => model_short(m),
        _ => "-".into(),
    }
}

/// Short state name (≤ 5 chars) for the table.
pub fn state_short(s: AgentState) -> &'static str {
    match s {
        AgentState::Arriving => "arriv",
        AgentState::Idle => "idle",
        AgentState::Thinking => "think",
        AgentState::Typing => "type",
        AgentState::Reading => "read",
        AgentState::Running => "run",
        AgentState::Delegating => "deleg",
        AgentState::NeedsYou => "NEED!",
        AgentState::Queued => "queue",
        AgentState::Done => "done",
        AgentState::Error => "ERR",
        AgentState::Leaving => "leave",
    }
}

pub fn state_name(s: AgentState) -> &'static str {
    match s {
        AgentState::Arriving => "arriving",
        AgentState::Idle => "idle",
        AgentState::Thinking => "thinking",
        AgentState::Typing => "typing",
        AgentState::Reading => "reading",
        AgentState::Running => "running",
        AgentState::Delegating => "delegating",
        AgentState::NeedsYou => "needs you",
        AgentState::Queued => "queued",
        AgentState::Done => "done",
        AgentState::Error => "error",
        AgentState::Leaving => "leaving",
    }
}

pub fn state_color(s: AgentState) -> Color {
    match s {
        AgentState::Arriving | AgentState::Leaving => Color::DarkGray,
        AgentState::Idle => Color::Gray,
        AgentState::Thinking => Color::Magenta,
        AgentState::Typing => Color::Green,
        AgentState::Reading => Color::Cyan,
        AgentState::Running => Color::LightBlue,
        AgentState::Delegating => Color::LightMagenta,
        AgentState::NeedsYou => Color::LightRed,
        AgentState::Queued => Color::Yellow,
        AgentState::Done => Color::LightGreen,
        AgentState::Error => Color::Red,
    }
}

pub fn kind_color(k: AgentKind) -> Color {
    match k {
        AgentKind::Claude => Color::Rgb(0xE0, 0x7A, 0x3F),   // orange
        AgentKind::ClaudeSub => Color::Rgb(0xF2, 0xB1, 0x34), // amber
        AgentKind::Agy => Color::Rgb(0x2F, 0xB3, 0xA8),       // teal
        AgentKind::Other => Color::Gray,
    }
}

pub fn level_color(l: AlertLevel) -> Color {
    match l {
        AlertLevel::Info => Color::Gray,
        AlertLevel::Warn => Color::Yellow,
        AlertLevel::Critical => Color::LightRed,
    }
}

pub fn level_tag(l: AlertLevel) -> &'static str {
    match l {
        AlertLevel::Info => "info",
        AlertLevel::Warn => "WARN",
        AlertLevel::Critical => "CRIT",
    }
}

/// agy-route PRESSURE (kern.memorystatus_vm_pressure_level): 1 normal, 2 warn, 4 critical.
pub fn pressure_label(p: Option<u8>) -> (&'static str, Color) {
    match p {
        None => ("unknown", Color::DarkGray),
        Some(v) if v >= 4 => ("critical", Color::LightRed),
        Some(v) if v >= 2 => ("warn", Color::Yellow),
        Some(_) => ("normal", Color::Green),
    }
}

/// Header counts: (claude sessions, claude subagents, agy calls, other).
pub fn kind_counts(scene: &Scene) -> (usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0);
    for a in scene.agents.values() {
        match a.kind {
            AgentKind::Claude => c.0 += 1,
            AgentKind::ClaudeSub => c.1 += 1,
            AgentKind::Agy => c.2 += 1,
            AgentKind::Other => c.3 += 1,
        }
    }
    c
}

/// Agents in display order with tree depth: roots (no parent, or parent not present) sorted by
/// start time then id, each followed by its descendants (depth-first).
pub fn agent_rows(scene: &Scene) -> Vec<(String, usize)> {
    let agents = &scene.agents;
    let is_root = |a: &Agent| match &a.parent {
        None => true,
        Some(p) => !agents.contains_key(p) || p == &a.id,
    };
    let sort_key = |a: &&Agent| (a.started_ms, a.id.clone());
    let mut roots: Vec<&Agent> = agents.values().filter(|a| is_root(a)).collect();
    roots.sort_by_key(sort_key);
    let mut out = Vec::with_capacity(agents.len());
    fn walk<'a>(
        a: &'a Agent,
        depth: usize,
        agents: &'a std::collections::BTreeMap<String, Agent>,
        out: &mut Vec<(String, usize)>,
    ) {
        if out.iter().any(|(id, _)| id == &a.id) || depth > 8 {
            return; // cycle guard
        }
        out.push((a.id.clone(), depth));
        let mut kids: Vec<&Agent> =
            agents.values().filter(|c| c.parent.as_deref() == Some(a.id.as_str()) && c.id != a.id).collect();
        kids.sort_by_key(|a| (a.started_ms, a.id.clone()));
        for k in kids {
            walk(k, depth + 1, agents, out);
        }
    }
    for r in roots {
        walk(r, 0, agents, &mut out);
    }
    // Anything unreachable (parent cycles) still gets listed.
    for a in agents.values() {
        if !out.iter().any(|(id, _)| id == &a.id) {
            out.push((a.id.clone(), 0));
        }
    }
    out
}

/// Tree-indented label: depth 0 "label", depth 1 "└label", depth 2 " └label".
pub fn tree_label(label: &str, depth: usize) -> String {
    if depth == 0 {
        label.to_string()
    } else {
        format!("{}└{}", " ".repeat(depth - 1), label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{agent, scene_with};

    #[test]
    fn truncation() {
        assert_eq!(trunc("hello", 10), "hello");
        assert_eq!(trunc("hello world", 6), "hello…");
        assert_eq!(trunc("abc", 0), "");
        assert_eq!(trunc("ünïcødé", 4), "ünï…");
    }

    #[test]
    fn elapsed_and_ago() {
        assert_eq!(fmt_elapsed_short(42_000), "42s");
        assert_eq!(fmt_elapsed_short(12 * 60_000 + 5), "12m");
        assert_eq!(fmt_elapsed_short(3 * 3_600_000), "3h");
        assert_eq!(fmt_elapsed_short(2 * 86_400_000), "2d");
        assert_eq!(fmt_elapsed_long(185_000), "3m05s");
        assert_eq!(fmt_elapsed_long(3_720_000), "1h02m");
        assert_eq!(fmt_ago(10_500, 10_000), "now");
        assert_eq!(fmt_ago(70_000, 10_000), "1m");
        assert_eq!(fmt_ago(0, 10_000), "now");
    }

    #[test]
    fn tokens_and_cost() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(123_456), "123k");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_cost_short(0.4231), "0.42");
        assert_eq!(fmt_cost_short(12.34), "12.3");
        assert_eq!(fmt_cost_short(123.4), "123");
        assert_eq!(fmt_cost(1.5), "$1.50");
    }

    #[test]
    fn labels_and_pressure() {
        assert_eq!(model_short("claude-opus-4-5"), "opus-4-5");
        assert_eq!(model_short("gemini-4-neon-high"), "neon-high");
        assert_eq!(pressure_label(Some(1)).0, "normal");
        assert_eq!(pressure_label(Some(2)).0, "warn");
        assert_eq!(pressure_label(Some(4)).0, "critical");
        assert_eq!(pressure_label(None).0, "unknown");
        for s in crate::app::testutil::ALL_STATES {
            assert!(state_short(s).chars().count() <= 5);
        }
        assert_eq!(tree_label("x", 0), "x");
        assert_eq!(tree_label("x", 1), "└x");
        assert_eq!(tree_label("x", 2), " └x");
    }

    #[test]
    fn rows_are_tree_ordered() {
        let mut a = agent("cc:a", AgentKind::Claude, None);
        a.started_ms = 10;
        let mut b = agent("cc:b", AgentKind::Claude, None);
        b.started_ms = 5;
        let mut s1 = agent("cc-sub:a/1", AgentKind::ClaudeSub, Some("cc:a"));
        s1.started_ms = 20;
        let s2 = agent("agy:9", AgentKind::Agy, Some("cc-sub:a/1"));
        let orphan = agent("cc-sub:z/1", AgentKind::ClaudeSub, Some("cc:z"));
        let scene = scene_with(vec![a, b, s1, s2, orphan]);
        let rows = agent_rows(&scene);
        let ids: Vec<(&str, usize)> = rows.iter().map(|(i, d)| (i.as_str(), *d)).collect();
        assert_eq!(
            ids,
            vec![("cc-sub:z/1", 0), ("cc:b", 0), ("cc:a", 0), ("cc-sub:a/1", 1), ("agy:9", 2)]
        );
        let (c, s, g, o) = kind_counts(&scene);
        assert_eq!((c, s, g, o), (2, 2, 1, 0));
    }
}
