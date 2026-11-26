//! Session metadata: reads `~/.claude/sessions/*.json` (`{pid, sessionId, entrypoint, cwd}`)
//! every 3 s, tracks live Claude processes + inclusive RSS via `ps`, and emits Gone when closed.
use office_core::model::AgentKind;
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

const POLL_EVERY: Duration = Duration::from_secs(3);
const MAX_FILES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub pid: u32,
    pub entrypoint: String,
    pub cwd: Option<String>,
}

/// `$OFFICE_SESSIONS_DIR`, else `~/.claude/sessions`.
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("OFFICE_SESSIONS_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => paths::claude_dir().join("sessions"),
    }
}

/// Parsed metadata for every readable session file in `dir`.
pub fn scan_meta(dir: &Path) -> Vec<SessionMeta> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten().take(MAX_FILES) {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(sid) = v.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            continue;
        };
        let pid = v.get("pid").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        let entrypoint = v.get("entrypoint").and_then(Value::as_str).unwrap_or("").to_string();
        let cwd = v.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        out.push(SessionMeta { id: sid.to_string(), pid, entrypoint, cwd });
    }
    out
}

/// `(cc:<sessionId>, entrypoint)` for every readable session file.
#[cfg(test)]
pub fn scan(dir: &Path) -> Vec<(String, String)> {
    scan_meta(dir)
        .into_iter()
        .filter(|m| !m.entrypoint.is_empty())
        .map(|m| (format!("cc:{}", m.id), m.entrypoint))
        .collect()
}

/// Parse `ps -axo pid=,ppid=,rss=,comm=` output and compute inclusive `self + descendants` RSS
/// in MB (`kb / 1024`) for every live PID present in `ps_text`.
pub fn rss_by_pid(ps_text: &str) -> HashMap<u32, u32> {
    let mut self_rss: HashMap<u32, u64> = HashMap::new();
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in ps_text.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid_s), Some(ppid_s), Some(rss_s)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(rss_kb)) = (pid_s.parse::<u32>(), ppid_s.parse::<u32>(), rss_s.parse::<u64>()) else {
            continue;
        };
        self_rss.insert(pid, rss_kb);
        kids.entry(ppid).or_default().push(pid);
    }

    let mut out = HashMap::with_capacity(self_rss.len());
    for (&root, &root_kb) in &self_rss {
        let mut kb = root_kb;
        let mut stack = vec![root];
        let mut visited = HashSet::from([root]);
        let mut guard = 0usize;
        while let Some(p) = stack.pop() {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            if let Some(children) = kids.get(&p) {
                for &c in children {
                    if visited.insert(c) {
                        if let Some(&ckb) = self_rss.get(&c) {
                            kb = kb.saturating_add(ckb);
                        }
                        stack.push(c);
                    }
                }
            }
        }
        out.insert(root, (kb / 1024) as u32);
    }
    out
}

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = sessions_dir();
    std::thread::Builder::new().name("sessions".into()).spawn(move || {
        let mut seen_live: HashMap<u32, String> = HashMap::new();
        loop {
            let metas = scan_meta(&dir);
            if !metas.is_empty() || !seen_live.is_empty() {
                let ps_out = Command::new("/bin/ps")
                    .args(["-axo", "pid=,ppid=,rss=,comm="])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output();
                if let Ok(out) = ps_out {
                    if out.status.success() {
                        let rss = rss_by_pid(&String::from_utf8_lossy(&out.stdout));
                        for meta in metas {
                            let id = format!("cc:{}", meta.id);
                            if meta.pid > 1 && rss.contains_key(&meta.pid) {
                                let rss_mb = rss[&meta.pid];
                                if tx
                                    .send(Event::now(
                                        SourceKind::ClaudeHook,
                                        EventKind::AgentSeen {
                                            id: id.clone(),
                                            kind: AgentKind::Claude,
                                            label: None,
                                            parent: None,
                                            cwd: meta.cwd,
                                            role: None,
                                            model: None,
                                            slot: None,
                                            pid: Some(meta.pid),
                                            resume_id: Some(meta.id),
                                        },
                                    ))
                                    .is_err()
                                {
                                    return;
                                }
                                if tx
                                    .send(Event::now(SourceKind::ClaudeHook, EventKind::Resources { id: id.clone(), rss_mb }))
                                    .is_err()
                                {
                                    return;
                                }
                                if !meta.entrypoint.is_empty()
                                    && tx
                                        .send(Event::now(
                                            SourceKind::ClaudeHook,
                                            EventKind::Entrypoint { id: id.clone(), entrypoint: meta.entrypoint },
                                        ))
                                        .is_err()
                                {
                                    return;
                                }
                                seen_live.insert(meta.pid, id);
                            } else if meta.pid == 0
                                && !meta.entrypoint.is_empty()
                                && tx
                                    .send(Event::now(
                                        SourceKind::ClaudeHook,
                                        EventKind::Entrypoint { id, entrypoint: meta.entrypoint },
                                    ))
                                    .is_err()
                            {
                                return;
                            }
                        }

                        let gone: Vec<(u32, String)> = seen_live
                            .iter()
                            .filter(|(pid, _)| !rss.contains_key(pid))
                            .map(|(pid, id)| (*pid, id.clone()))
                            .collect();
                        for (pid, id) in gone {
                            seen_live.remove(&pid);
                            if tx.send(Event::now(SourceKind::ClaudeHook, EventKind::Gone { id })).is_err() {
                                return;
                            }
                        }
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

    #[test]
    fn scans_session_files() {
        let d = std::env::temp_dir().join(format!("office-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
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

    #[test]
    fn scans_session_meta_with_optional_fields() {
        let d = std::env::temp_dir().join(format!("office-sessions-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("1.json"), r#"{"pid":42,"sessionId":"abc","entrypoint":"claude-desktop","cwd":"/x/repo"}"#).unwrap();
        std::fs::write(d.join("2.json"), r#"{"sessionId":"def","entrypoint":"cli"}"#).unwrap();
        std::fs::write(d.join("3.json"), r#"{"pid":99,"sessionId":"ghi"}"#).unwrap();
        std::fs::write(d.join("4.json"), r#"{"pid":4}"#).unwrap();
        let mut metas = scan_meta(&d);
        metas.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            metas,
            vec![
                SessionMeta { id: "abc".into(), pid: 42, entrypoint: "claude-desktop".into(), cwd: Some("/x/repo".into()) },
                SessionMeta { id: "def".into(), pid: 0, entrypoint: "cli".into(), cwd: None },
                SessionMeta { id: "ghi".into(), pid: 99, entrypoint: String::new(), cwd: None },
            ]
        );
        let mut pairs = scan(&d);
        pairs.sort();
        assert_eq!(pairs, vec![("cc:abc".into(), "claude-desktop".into()), ("cc:def".into(), "cli".into())]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn computes_inclusive_rss_and_guards_cycles() {
        let ps = "\
  100     1  2048 claude
  101   100  1024 node
  102   101  4096 rg
  200     1   512 zsh
  300   301  1024 loop_a
  301   300  2048 loop_b
";
        let rss = rss_by_pid(ps);
        assert_eq!(rss.get(&100), Some(&7)); // 2 + 1 + 4 MB
        assert_eq!(rss.get(&101), Some(&5)); // 1 + 4 MB
        assert_eq!(rss.get(&102), Some(&4));
        assert_eq!(rss.get(&200), Some(&0)); // < 1024 KB still present as 0 MB
        assert_eq!(rss.get(&300), Some(&3)); // cycle-safe: 1 + 2 MB
        assert_eq!(rss.get(&301), Some(&3));
        assert_eq!(rss.get(&999), None);
    }
}
