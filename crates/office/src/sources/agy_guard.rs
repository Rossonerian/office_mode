//! Security Desk watcher: tails `~/.claude/run/agy-guard.log.jsonl` (one JSON line per agy-guard
//! decision) and reports denials as an alert plus `EventKind::GuardDecision`. Starts at the end
//! of the file on first sight (no history replay). Only the denial reason is kept, never the command.
use super::short;
use office_core::model::{Alert, AlertLevel};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

const POLL_EVERY: Duration = Duration::from_secs(1);

/// `$OFFICE_RUN_DIR/agy-guard.log.jsonl`, else `~/.claude/run/agy-guard.log.jsonl`.
pub fn log_path() -> PathBuf {
    paths::run_dir().join("agy-guard.log.jsonl")
}

/// Incremental reader: remembers the offset and a partial last line.
#[derive(Default)]
pub struct LineTail {
    offset: Option<u64>,
    partial: Vec<u8>,
}

impl LineTail {
    /// New complete lines since the last call. The first call only records the end of the file.
    pub fn read(&mut self, path: &Path) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let start = match self.offset {
            None => {
                self.offset = Some(len);
                return Vec::new();
            }
            Some(o) if o > len => {
                self.partial.clear();
                0
            }
            Some(o) => o,
        };
        if f.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        let _ = f.take(2 << 20).read_to_end(&mut buf);
        self.offset = Some(start + buf.len() as u64);
        self.partial.extend_from_slice(&buf);
        let Some(nl) = self.partial.iter().rposition(|b| *b == b'\n') else {
            if self.partial.len() > (1 << 20) {
                self.partial.clear();
            }
            return Vec::new();
        };
        let done: Vec<u8> = self.partial.drain(..=nl).collect();
        String::from_utf8_lossy(&done).lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
    }
}

/// One guard log line -> events (empty for allow decisions and junk).
pub fn parse_line(line: &str, now: u64) -> Vec<EventKind> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    if v.get("decision").and_then(Value::as_str) != Some("deny") {
        return Vec::new();
    }
    let g = |k: &str, d: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(d).to_string();
    let (agent, tool, reason) = (g("agent", "agent"), g("tool", "tool"), short(&g("reason", "denied"), 120));
    vec![
        EventKind::Alert(Alert {
            at_ms: now,
            level: AlertLevel::Warn,
            agent: None,
            code: "guard_deny".into(),
            message: format!("Security Desk blocked {agent} {tool}: {reason}"),
        }),
        EventKind::GuardDecision { agent, tool, reason, deny: true },
    ]
}

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let log = log_path();
    std::thread::Builder::new().name("agy-guard".into()).spawn(move || {
        let mut tail = LineTail::default();
        loop {
            for line in tail.read(&log) {
                for k in parse_line(&line, paths::now_ms()) {
                    if tx.send(Event::now(SourceKind::AgyRouter, k)).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn deny_makes_alert_and_decision_allow_is_ignored() {
        let deny = r#"{"ts":"2026-11-25T14:12:51.463Z","decision":"deny","reason":"'~/.claude/bin/rosso' is not on the read-only allowlist","agent":"agy-tester","tool":"Bash","cmd":"secret command text"}"#;
        let ks = parse_line(deny, 5);
        assert_eq!(ks.len(), 2);
        match &ks[0] {
            EventKind::Alert(a) => {
                assert_eq!(a.code, "guard_deny");
                assert_eq!(a.level, AlertLevel::Warn);
                assert!(a.message.starts_with("Security Desk blocked agy-tester Bash: "));
                assert!(!a.message.contains("secret command"));
            }
            k => panic!("{k:?}"),
        }
        assert!(matches!(&ks[1], EventKind::GuardDecision { agent, deny: true, .. } if agent == "agy-tester"));
        assert!(parse_line(r#"{"decision":"allow","agent":"a","tool":"Bash"}"#, 1).is_empty());
        assert!(parse_line("not json", 1).is_empty());
    }

    #[test]
    fn tail_starts_at_end_and_reads_only_complete_new_lines() {
        let d = std::env::temp_dir().join(format!("office-guard-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("g.jsonl");
        std::fs::write(&p, "{\"decision\":\"deny\",\"reason\":\"old\"}\n").unwrap();
        let mut t = LineTail::default();
        assert!(t.read(&p).is_empty());
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{{\"decision\":\"deny\",\"reason\":\"new\"}}\n{{\"decision\":\"de").unwrap();
        let l = t.read(&p);
        assert_eq!(l.len(), 1);
        assert!(l[0].contains("new"));
        writeln!(f, "ny\"}}").unwrap();
        assert_eq!(t.read(&p).len(), 1);
        assert!(t.read(&p).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
