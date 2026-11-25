//! Session metadata: reads `~/.claude/sessions/*.json` (`{sessionId, entrypoint}`) every 10 s and
//! reports each session's entrypoint ("claude-desktop" / "cli") for the DESKTOP / CLI badge.
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

const POLL_EVERY: Duration = Duration::from_secs(10);
const MAX_FILES: usize = 256;

/// `$OFFICE_SESSIONS_DIR`, else `~/.claude/sessions`.
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("OFFICE_SESSIONS_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => paths::claude_dir().join("sessions"),
    }
}

/// `(cc:<sessionId>, entrypoint)` for every readable session file.
pub fn scan(dir: &Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten().take(MAX_FILES) {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let (Some(sid), Some(ep)) = (v.get("sessionId").and_then(Value::as_str), v.get("entrypoint").and_then(Value::as_str)) else {
            continue;
        };
        if !sid.is_empty() && !ep.is_empty() {
            out.push((format!("cc:{sid}"), ep.to_string()));
        }
    }
    out
}

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = sessions_dir();
    std::thread::Builder::new().name("sessions".into()).spawn(move || loop {
        for (id, entrypoint) in scan(&dir) {
            if tx.send(Event::now(SourceKind::ClaudeHook, EventKind::Entrypoint { id, entrypoint })).is_err() {
                return;
            }
        }
        std::thread::sleep(POLL_EVERY);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_session_files() {
        let d = std::env::temp_dir().join(format!("office-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("1.json"), r#"{"pid":1,"sessionId":"abc","entrypoint":"claude-desktop","cwd":"/x"}"#).unwrap();
        std::fs::write(d.join("2.json"), r#"{"pid":2,"sessionId":"def","entrypoint":"cli"}"#).unwrap();
        std::fs::write(d.join("3.json"), "junk").unwrap();
        std::fs::write(d.join("4.json"), r#"{"pid":4}"#).unwrap();
        let mut v = scan(&d);
        v.sort();
        assert_eq!(v, vec![("cc:abc".to_string(), "claude-desktop".to_string()), ("cc:def".to_string(), "cli".to_string())]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
