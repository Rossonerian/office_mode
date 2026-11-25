//! Workstream A. Background producers of office_core::Event.
use office_core::model::{Alert, AlertLevel};
use office_core::{config::Config, Event, EventKind, SourceKind};
use std::sync::mpsc::Sender;

pub mod agy_router;
pub mod claude_transcripts;
pub mod control;
pub mod hook_socket;
pub mod reports;
pub mod rosso;

/// Start every source on its own thread(s): hook socket listener, Claude transcript tailer, agy router scanner.
/// Must never panic the caller; source failures become Alert events.
pub fn spawn_all(cfg: &Config, tx: Sender<Event>) -> Option<hook_socket::SocketGuard> {
    let guard = match hook_socket::spawn(cfg, tx.clone()) {
        Ok(g) => Some(g),
        Err(e) => {
            source_alert(&tx, SourceKind::ClaudeHook, "hook_socket", format!("hook socket unavailable: {e}"));
            None
        }
    };
    if let Err(e) = claude_transcripts::spawn(cfg, tx.clone()) {
        source_alert(&tx, SourceKind::ClaudeTranscript, "transcripts", format!("transcript tailer failed: {e}"));
    }
    if let Err(e) = agy_router::spawn(cfg, tx.clone()) {
        source_alert(&tx, SourceKind::AgyRouter, "agy_router", format!("agy router scanner failed: {e}"));
    }
    if let Err(e) = rosso::spawn(cfg, tx.clone()) {
        source_alert(&tx, SourceKind::Rosso, "rosso", format!("rosso watcher failed: {e}"));
    }
    guard
}

pub(crate) fn source_alert(tx: &Sender<Event>, source: SourceKind, code: &str, message: String) {
    let a = Alert { at_ms: office_core::paths::now_ms(), level: AlertLevel::Warn, agent: None, code: code.into(), message };
    let _ = tx.send(Event::now(source, EventKind::Alert(a)));
}

/// Last path component (for labels like "cc·DICE").
pub(crate) fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(t).to_string()
}

/// First `n` chars of a single-line version of `s` (adds "…" when cut).
pub(crate) fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        let mut t: String = one.chars().take(n).collect();
        t.push('…');
        t
    }
}
