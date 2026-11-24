//! agy-route watcher: `ps` scan of live routers (1 s), slot/worker lock files, dry-run capacity (5 s),
//! and the audit log tail (~/.claude/run/agy-route.log.jsonl) for failed calls.
use super::basename;
use office_core::model::{AgentKind, AgentState, Alert, AlertLevel, RouterStatus};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SCAN_EVERY: Duration = Duration::from_secs(1);
const DRY_RUN_EVERY: Duration = Duration::from_secs(5);
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(4);

/// One `ps -axo pid=,ppid=,rss=,command=` row.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub rss_kb: u64,
    pub cmd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouterProc {
    pub pid: u32,
    pub role: String,
    pub cwd: Option<String>,
    pub rss_mb: u32,
}

/// Shared between the scan and audit threads.
#[derive(Default)]
struct Shared {
    /// role -> model as last seen in the audit log.
    models: HashMap<String, String>,
    /// routers without a slot (queued).
    queued: u32,
    /// (role, cwd) -> agent id of live/recent routers, to attribute audit alerts.
    recent: HashMap<(String, String), String>,
}

pub fn spawn(cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let run_dir = paths::agy_run_dir();

    {
        let (cfg, tx, shared, run_dir) = (cfg.clone(), tx.clone(), shared.clone(), run_dir.clone());
        std::thread::Builder::new().name("agy-scan".into()).spawn(move || scan_loop(cfg, tx, shared, run_dir))?;
    }
    {
        let (route, tx, shared) = (cfg.agy_route.clone(), tx.clone(), shared.clone());
        std::thread::Builder::new().name("agy-dry-run".into()).spawn(move || loop {
            if Path::new(&route).exists() {
                if let Some(text) = run_dry(&route) {
                    let mut st = parse_dry_run(&text);
                    st.queued = shared.lock().map(|s| s.queued).unwrap_or(0);
                    if tx.send(Event::now(SourceKind::AgyRouter, EventKind::Router(st))).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(DRY_RUN_EVERY);
        })?;
    }
    {
        let log = run_dir.join("agy-route.log.jsonl");
        std::thread::Builder::new().name("agy-audit".into()).spawn(move || audit_loop(log, tx, shared))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- ps scan

pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let rss_kb = it.next()?.parse().ok()?;
            let cmd = it.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, rss_kb, cmd })
        })
        .collect()
}

fn flag(cmd: &str, name: &str) -> Option<String> {
    let mut it = cmd.split_whitespace();
    while let Some(t) = it.next() {
        if t == name {
            return it.next().map(str::to_string);
        }
        if let Some(v) = t.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v.to_string());
        }
    }
    None
}

/// Live routers: `bun … agy-route --role <r> …` (not --dry-run), with summed descendant RSS.
pub fn find_routers(procs: &[Proc]) -> Vec<RouterProc> {
    let mut kids: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for p in procs {
        kids.entry(p.ppid).or_default().push(p);
    }
    procs
        .iter()
        .filter(|p| {
            let first = p.cmd.split_whitespace().next().unwrap_or("");
            basename(first) == "bun" && p.cmd.contains("agy-route") && p.cmd.contains("--role") && !p.cmd.contains("--dry-run")
        })
        .map(|p| {
            let mut kb = 0u64;
            let mut stack = vec![p.pid];
            let mut guard = 0;
            while let Some(x) = stack.pop() {
                guard += 1;
                if guard > 10_000 {
                    break;
                }
                for k in kids.get(&x).into_iter().flatten() {
                    kb += k.rss_kb;
                    stack.push(k.pid);
                }
            }
            RouterProc {
                pid: p.pid,
                role: flag(&p.cmd, "--role").unwrap_or_else(|| "?".into()),
                cwd: flag(&p.cmd, "--cwd"),
                rss_mb: (kb / 1024) as u32,
            }
        })
        .collect()
}

/// pid -> slot index from agy-route.slot<i>.lock (each holds the router pid).
pub fn read_slots(run_dir: &Path) -> HashMap<u32, u32> {
    let mut m = HashMap::new();
    let Ok(rd) = std::fs::read_dir(run_dir) else { return m };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(i) = name.strip_prefix("agy-route.slot").and_then(|r| r.strip_suffix(".lock")).and_then(|n| n.parse().ok())
        else {
            continue;
        };
        if let Some(pid) = std::fs::read_to_string(e.path()).ok().and_then(|s| s.trim().parse().ok()) {
            m.insert(pid, i);
        }
    }
    m
}

/// "gemini-4-hydrogen-low" -> "hydrogen-low", "gemini-3.8-flash-medium" -> "flash-medium".
pub fn short_model(m: &str) -> String {
    let rest = m.strip_prefix("gemini-").unwrap_or(m);
    match rest.split_once('-') {
        Some((ver, tail)) if ver.chars().next().is_some_and(|c| c.is_ascii_digit()) => tail.to_string(),
        _ => rest.to_string(),
    }
}

fn scan_loop(cfg: Config, tx: Sender<Event>, shared: Arc<Mutex<Shared>>, run_dir: PathBuf) {
    // pid -> (slot, rss_mb, running)
    let mut live: BTreeMap<u32, (Option<u32>, u32, bool)> = BTreeMap::new();
    loop {
        let out = Command::new("ps").args(["-axo", "pid=,ppid=,rss=,command="]).stderr(Stdio::null()).output();
        if let Ok(out) = out {
            let procs = parse_ps(&String::from_utf8_lossy(&out.stdout));
            let routers = find_routers(&procs);
            let slots = read_slots(&run_dir);
            let mut evs = Vec::new();
            let mut queued = 0;
            let models = shared.lock().map(|s| s.models.clone()).unwrap_or_default();
            for r in &routers {
                let id = format!("agy:{}", r.pid);
                let slot = slots.get(&r.pid).copied();
                let model = models.get(&r.role).cloned().or_else(|| cfg.agy_models.get(&r.role).cloned());
                let running = slot.is_some() || r.rss_mb > 0;
                if !running {
                    queued += 1;
                }
                let prev = live.get(&r.pid).copied();
                if prev.map(|p| (p.0, p.2)) != Some((slot, running)) {
                    let label = match &model {
                        Some(m) => format!("{}·{}", r.role, short_model(m)),
                        None => r.role.clone(),
                    };
                    evs.push(EventKind::AgentSeen {
                        id: id.clone(),
                        kind: AgentKind::Agy,
                        label: Some(label),
                        parent: None,
                        cwd: r.cwd.clone(),
                        role: Some(r.role.clone()),
                        model,
                        slot,
                        pid: Some(r.pid),
                        resume_id: None,
                    });
                    let (state, act) = if !running {
                        (AgentState::Queued, "waiting for a slot".to_string())
                    } else if r.role == "worker" {
                        (AgentState::Typing, format!("editing {}", r.cwd.as_deref().map(basename).unwrap_or_default()))
                    } else {
                        (AgentState::Thinking, format!("{} {}", r.role, r.cwd.as_deref().map(basename).unwrap_or_default()))
                    };
                    evs.push(EventKind::State { id: id.clone(), state, activity: Some(act.trim().to_string()) });
                    if let (Ok(mut s), Some(c)) = (shared.lock(), &r.cwd) {
                        s.recent.insert((r.role.clone(), c.clone()), id.clone());
                    }
                }
                if prev.map(|p| p.1) != Some(r.rss_mb) {
                    evs.push(EventKind::Resources { id: id.clone(), rss_mb: r.rss_mb });
                }
                live.insert(r.pid, (slot, r.rss_mb, running));
            }
            let gone: Vec<u32> = live.keys().filter(|p| !routers.iter().any(|r| r.pid == **p)).copied().collect();
            for pid in gone {
                live.remove(&pid);
                evs.push(EventKind::Gone { id: format!("agy:{pid}") });
            }
            if let Ok(mut s) = shared.lock() {
                s.queued = queued;
                if s.recent.len() > 256 {
                    s.recent.clear();
                }
            }
            for k in evs {
                if tx.send(Event::now(SourceKind::AgyRouter, k)).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(SCAN_EVERY);
    }
}

// ---------------------------------------------------------------- dry run

fn run_dry(route: &str) -> Option<String> {
    let mut child = Command::new(route)
        .args(["--role", "fallback", "--prompt", "x", "--dry-run"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < DRY_RUN_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut s = String::new();
    child.stdout.take()?.read_to_string(&mut s).ok()?;
    Some(s)
}

fn first_num<T: std::str::FromStr>(s: &str) -> Option<T> {
    let t: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    t.parse().ok()
}

/// Parse CAPACITY/FREE_MB/PER_PROC_MB/PRESSURE/SLOTS/LIMIT/MAX_CONCURRENT from the dry-run block.
pub fn parse_dry_run(text: &str) -> RouterStatus {
    let mut st = RouterStatus::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim() {
            "CAPACITY" => st.capacity = first_num(v),
            "MAX_CONCURRENT" => st.max_concurrent = first_num(v),
            "FREE_MB" => st.free_mb = first_num(v),
            "PER_PROC_MB" => st.per_proc_mb = first_num(v),
            "PRESSURE" => st.pressure = first_num(v),
            "SLOTS" => {
                // "0/5 slots busy"
                st.busy_slots = first_num(v).unwrap_or(0);
                if st.max_concurrent.is_none() {
                    st.max_concurrent = v.split_once('/').and_then(|(_, r)| first_num(r));
                }
            }
            "LIMIT" if !v.is_empty() => st.limit_text = Some(v.to_string()),
            "RESPONSE" => break,
            _ => {}
        }
    }
    st
}

// ---------------------------------------------------------------- audit log

/// Turn one audit line into an alert (failed calls / needs input). Also returns (role, model).
pub fn audit_alert(v: &Value, agent: Option<String>, now: u64) -> Option<Alert> {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("?");
    let cwd = v.get("cwd").and_then(Value::as_str).map(basename).unwrap_or_default();
    let who = format!("agy {role}{}", if cwd.is_empty() { String::new() } else { format!(" ({cwd})") });
    if let Some(code) = v.get("errorCode").and_then(Value::as_str) {
        let (c, level, what) = match code {
            "TOOL_DENIED" => ("tool_denied", AlertLevel::Warn, "tool call denied"),
            "TIMEOUT" => ("timeout", AlertLevel::Critical, "timed out"),
            "EMPTY_RESPONSE" => ("empty_response", AlertLevel::Warn, "empty response"),
            "BUSY" => ("router_busy", AlertLevel::Warn, "router busy (queued too long)"),
            _ => ("agy_error", AlertLevel::Warn, code),
        };
        return Some(Alert { at_ms: now, level, agent, code: c.into(), message: format!("{who}: {what}") });
    }
    if v.get("needsInput").and_then(Value::as_bool) == Some(true) {
        return Some(Alert { at_ms: now, level: AlertLevel::Warn, agent, code: "needs_you".into(), message: format!("{who}: NEEDS_USER_INPUT") });
    }
    None
}

fn audit_loop(log: PathBuf, tx: Sender<Event>, shared: Arc<Mutex<Shared>>) {
    let mut offset: Option<u64> = None;
    let mut partial = Vec::new();
    loop {
        if let Ok(mut f) = std::fs::File::open(&log) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let start = match offset {
                // first sight: seed models from the last 64 KB, don't replay old alerts
                None => len.saturating_sub(64 * 1024),
                Some(o) if o > len => 0,
                Some(o) => o,
            };
            let replay = offset.is_some();
            if f.seek(SeekFrom::Start(start)).is_ok() {
                let mut buf = Vec::new();
                let _ = f.take(4 << 20).read_to_end(&mut buf);
                offset = Some(start + buf.len() as u64);
                partial.extend_from_slice(&buf);
                if let Some(nl) = partial.iter().rposition(|b| *b == b'\n') {
                    let lines: Vec<u8> = partial.drain(..=nl).collect();
                    for l in lines.split(|b| *b == b'\n') {
                        let Ok(v) = serde_json::from_slice::<Value>(l) else { continue };
                        let role = v.get("role").and_then(Value::as_str).unwrap_or("").to_string();
                        let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
                        let mut agent = None;
                        if let Ok(mut s) = shared.lock() {
                            if let Some(m) = v.get("model").and_then(Value::as_str) {
                                s.models.insert(role.clone(), m.to_string());
                            }
                            agent = s.recent.get(&(role, cwd)).cloned();
                        }
                        if replay {
                            if let Some(a) = audit_alert(&v, agent, paths::now_ms()) {
                                if tx.send(Event::now(SourceKind::AgyRouter, EventKind::Alert(a))).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                if !replay {
                    partial.clear(); // the seed window may start mid-line
                }
            }
        }
        std::thread::sleep(SCAN_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dry_run_block() {
        let t = "STATUS: DRY_RUN\nMAX_CONCURRENT: 5\nSLOTS: 1/5 slots busy\nCAPACITY: 2\nFREE_MB: 1571\nPER_PROC_MB: 300\nPRESSURE: 2 (warn)\nLIMIT: memory pressure warn (level 2) -> capacity 2\nRESPONSE:\nCAPACITY: 99\n";
        let s = parse_dry_run(t);
        assert_eq!(s.capacity, Some(2));
        assert_eq!(s.max_concurrent, Some(5));
        assert_eq!(s.busy_slots, 1);
        assert_eq!(s.free_mb, Some(1571));
        assert_eq!(s.per_proc_mb, Some(300));
        assert_eq!(s.pressure, Some(2));
        assert_eq!(s.limit_text.as_deref(), Some("memory pressure warn (level 2) -> capacity 2"));
    }

    #[test]
    fn finds_routers_and_descendant_rss() {
        let ps = "\
  100     1  50000 /opt/homebrew/bin/bun /Users/u/.claude/bin/agy-route --role worker --cwd /p/repo --prompt-file /tmp/x
  101   100 204800 /opt/homebrew/bin/agy -p x --model gemini-4-hydrogen-low
  102   101 102400 node helper
  200     1  40000 /opt/homebrew/bin/bun /Users/u/.claude/bin/agy-route --role fallback --prompt x --dry-run
  300     1   1000 /bin/zsh -c grep agy-route --role
";
        let r = find_routers(&parse_ps(ps));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], RouterProc { pid: 100, role: "worker".into(), cwd: Some("/p/repo".into()), rss_mb: 300 });
    }

    #[test]
    fn short_models_and_audit_alerts() {
        assert_eq!(short_model("gemini-4-hydrogen-low"), "hydrogen-low");
        assert_eq!(short_model("gemini-3.8-flash-medium"), "flash-medium");
        let v: Value = serde_json::from_str(r#"{"role":"worker","cwd":"/p/DICE","errorCode":"TIMEOUT"}"#).unwrap();
        let a = audit_alert(&v, None, 1).unwrap();
        assert_eq!(a.code, "timeout");
        assert_eq!(a.level, AlertLevel::Critical);
        let ok: Value = serde_json::from_str(r#"{"role":"worker","errorCode":null,"needsInput":false}"#).unwrap();
        assert!(audit_alert(&ok, None, 1).is_none());
    }

    #[test]
    fn reads_slot_locks() {
        let d = std::env::temp_dir().join(format!("office-slots-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agy-route.slot2.lock"), "4242").unwrap();
        std::fs::write(d.join("agy-route.worker-abc.lock"), "4242").unwrap();
        let m = read_slots(&d);
        assert_eq!(m.get(&4242), Some(&2));
        assert_eq!(m.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
