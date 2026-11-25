//! agy-route watcher: `ps` scan of live routers (1 s), slot/worker lock files, dry-run capacity (5 s),
//! and the audit log tail (~/.claude/run/agy-route.log.jsonl) for failed calls.
use super::basename;
use office_core::model::{AgentKind, AgentState, Alert, AlertLevel, FailureInfo, QueueTicket, ReportKind, RouterExtra, RouterStatus};
use office_core::{config::Config, paths, AgentId, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// `--domain` for manager calls (frontend, backend, …).
    pub domain: Option<String>,
    pub cwd: Option<String>,
    pub rss_mb: u32,
    /// `--rosso-job <job>`: the ROSSO job this call works for.
    pub rosso_job: Option<String>,
    /// `--as "<title>"`: the ROSSO office title this call plays.
    pub rosso_as: Option<String>,
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
    /// role -> model from `--status` ROLE_MODELS.
    role_models: HashMap<String, String>,
    /// Last orphan count alerted on.
    orphans_alerted: u32,
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
                    let extra = parse_router_extra(&text);
                    let mut evs = vec![EventKind::Router(st)];
                    if let Ok(mut sh) = shared.lock() {
                        sh.role_models = extra.role_models.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        if let Some(a) = orphan_alert(extra.orphans, sh.orphans_alerted, paths::now_ms()) {
                            evs.push(EventKind::Alert(a));
                        }
                        sh.orphans_alerted = extra.orphans;
                    }
                    evs.push(EventKind::RouterExtra(extra));
                    for k in evs {
                        if tx.send(Event::now(SourceKind::AgyRouter, k)).is_err() {
                            return;
                        }
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
    let mut it = cmd.split_whitespace().peekable();
    while let Some(t) = it.next() {
        if t == name {
            if name == "--cwd" || name == "--as" {
                let mut parts = Vec::new();
                while let Some(&next) = it.peek() {
                    if next.starts_with("--") {
                        break;
                    }
                    parts.push(it.next().unwrap());
                }
                return (!parts.is_empty()).then(|| parts.join(" "));
            }
            return it.next().map(str::to_string);
        }
        if let Some(v) = t.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
            return Some(v.to_string());
        }
    }
    None
}

fn has_flag(cmd: &str, name: &str) -> bool {
    let mut it = cmd.split_whitespace();
    while let Some(t) = it.next() {
        if t == name || t.starts_with(&format!("{name}=")) {
            return true;
        }
        if t == "--prompt" || t == "--prompt-file" {
            let _ = it.next();
        }
    }
    false
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
            basename(first) == "bun"
                && p.cmd.contains("agy-route")
                && has_flag(&p.cmd, "--role")
                && !has_flag(&p.cmd, "--dry-run")
                && !has_flag(&p.cmd, "--status")
        })
        .map(|p| {
            let mut kb = 0u64;
            let mut stack = vec![p.pid];
            let mut visited = HashSet::from([p.pid]);
            let mut guard = 0;
            while let Some(x) = stack.pop() {
                guard += 1;
                if guard > 10_000 {
                    break;
                }
                for k in kids.get(&x).into_iter().flatten() {
                    if visited.insert(k.pid) {
                        kb += k.rss_kb;
                        stack.push(k.pid);
                    }
                }
            }
            RouterProc {
                pid: p.pid,
                role: flag(&p.cmd, "--role").unwrap_or_else(|| "?".into()),
                domain: flag(&p.cmd, "--domain"),
                cwd: flag(&p.cmd, "--cwd"),
                rss_mb: (kb / 1024) as u32,
                rosso_job: flag(&p.cmd, "--rosso-job"),
                rosso_as: flag(&p.cmd, "--as"),
            }
        })
        .collect()
}

/// Parse the first whitespace token on line 1 of a lock file as a PID.
fn parse_first_pid(path: &Path) -> Option<u32> {
    let f = std::fs::File::open(path).ok()?;
    let mut s = String::new();
    f.take(4096).read_to_string(&mut s).ok()?;
    s.lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|p| p.parse().ok())
}

/// pid -> slot index from agy-route.slot<i>.lock (line 1 starts with the router pid).
pub fn read_slots(run_dir: &Path) -> HashMap<u32, u32> {
    let mut m = HashMap::new();
    let Ok(rd) = std::fs::read_dir(run_dir) else { return m };
    for e in rd.flatten().take(16_384) {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(i) = name.strip_prefix("agy-route.slot").and_then(|r| r.strip_suffix(".lock")).and_then(|n| n.parse().ok())
        else {
            continue;
        };
        if let Some(pid) = parse_first_pid(&e.path()) {
            m.insert(pid, i);
        }
    }
    m
}

fn parse_ticket_name(name: &str) -> Option<(u64, u32)> {
    let (ms_part, pid_part) = name.split_once('-')?;
    if ms_part.len() < 13
        || pid_part.is_empty()
        || !ms_part.bytes().all(|b| b.is_ascii_digit())
        || !pid_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((ms_part.parse().ok()?, pid_part.parse().ok()?))
}

/// Read FIFO queue ticket files `<run_dir>/queue/<13+-digit-ms>-<pid>`, sorted ascending by `(ms, pid)`.
pub fn read_queue_tickets(run_dir: &Path) -> Vec<(u64, u32)> {
    let Ok(rd) = std::fs::read_dir(run_dir.join("queue")) else { return Vec::new() };
    let mut out: Vec<(u64, u32)> = rd
        .flatten()
        .take(4096)
        .filter_map(|e| parse_ticket_name(&e.file_name().to_string_lossy()))
        .collect();
    out.sort_unstable();
    out
}

/// Read PIDs from `<run_dir>/agy-route.worker-*.lock` (first whitespace token on line 1).
pub fn read_worker_locks(run_dir: &Path) -> HashSet<u32> {
    let mut out = HashSet::new();
    let Ok(rd) = std::fs::read_dir(run_dir) else { return out };
    for e in rd.flatten().take(16_384) {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("agy-route.worker-") && s.ends_with(".lock") {
            if let Some(pid) = parse_first_pid(&e.path()) {
                out.insert(pid);
            }
        }
    }
    out
}

/// Whether a queued worker router is waiting on the same repo as another running/locked worker.
pub fn is_repo_busy(r: &RouterProc, routers: &[RouterProc], slots: &HashMap<u32, u32>, worker_locks: &HashSet<u32>) -> bool {
    if r.role != "worker" {
        return false;
    }
    if let Some(my_cwd) = r.cwd.as_deref().filter(|c| !c.is_empty()).map(canon_cwd) {
        let same_repo_running = routers.iter().any(|other| {
            other.pid != r.pid
                && other.role == "worker"
                && (slots.contains_key(&other.pid) || other.rss_mb > 0)
                && other.cwd.as_deref().filter(|c| !c.is_empty()).map(canon_cwd).as_deref() == Some(my_cwd.as_str())
        });
        if same_repo_running {
            return true;
        }
    }
    slots.get(&r.pid).is_none()
        && worker_locks.iter().any(|&p| p != r.pid && routers.iter().any(|o| o.pid == p && o.role == "worker"))
}

/// "gemini-4-hydrogen-low" -> "hydrogen-low", "gemini-3.8-flash-medium" -> "flash-medium".
pub fn short_model(m: &str) -> String {
    let rest = m.strip_prefix("gemini-").unwrap_or(m);
    match rest.split_once('-') {
        Some((ver, tail)) if ver.chars().next().is_some_and(|c| c.is_ascii_digit()) => tail.to_string(),
        _ => rest.to_string(),
    }
}

fn has_any_lock(run_dir: &Path) -> bool {
    let has_lock = std::fs::read_dir(run_dir).ok().is_some_and(|rd| {
        rd.flatten().take(16_384).any(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.starts_with("agy-route.") && s.ends_with(".lock")
        })
    });
    if has_lock {
        return true;
    }
    std::fs::read_dir(run_dir.join("queue"))
        .ok()
        .is_some_and(|rd| rd.flatten().take(4096).any(|e| parse_ticket_name(&e.file_name().to_string_lossy()).is_some()))
}

fn canon_cwd(p: &str) -> String {
    std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
}

#[derive(Clone, PartialEq)]
struct LiveRouter {
    slot: Option<u32>,
    rss_mb: u32,
    running: bool,
    model: Option<String>,
    queue_key: Option<(u32, bool)>,
}

fn scan_loop(cfg: Config, tx: Sender<Event>, shared: Arc<Mutex<Shared>>, run_dir: PathBuf) {
    let mut live: BTreeMap<u32, LiveRouter> = BTreeMap::new();
    let mut fallback_since: HashMap<u32, u64> = HashMap::new();
    let mut last_tickets: BTreeMap<AgentId, QueueTicket> = BTreeMap::new();
    let mut tick: u64 = 0;
    loop {
        let idle = live.is_empty() && !has_any_lock(&run_dir);
        if !idle || tick % 3 == 0 {
            let out = Command::new("ps").args(["-axo", "pid=,ppid=,rss=,command="]).stderr(Stdio::null()).output();
            if let Ok(out) = out {
                let procs = parse_ps(&String::from_utf8_lossy(&out.stdout));
                let routers = find_routers(&procs);
                let slots = read_slots(&run_dir);
                let tickets = read_queue_tickets(&run_dir);
                let worker_locks = read_worker_locks(&run_dir);
                let mut ticket_by_pid: HashMap<u32, u64> = HashMap::new();
                for (ms, pid) in tickets {
                    ticket_by_pid.entry(pid).or_insert(ms);
                }

                let mut queued_routers: Vec<(u64, u32, &RouterProc)> = routers
                    .iter()
                    .filter(|r| !slots.contains_key(&r.pid) && r.rss_mb == 0)
                    .map(|r| (ticket_by_pid.get(&r.pid).copied().unwrap_or(u64::MAX), r.pid, r))
                    .collect();
                queued_routers.sort_unstable_by_key(|&(ms, pid, _)| (ms, pid));

                let mut queue_tickets: BTreeMap<AgentId, QueueTicket> = BTreeMap::new();
                let mut queued_by_pid: HashMap<u32, QueueTicket> = HashMap::new();
                for (idx, (ticket_ms, pid, r)) in queued_routers.into_iter().enumerate() {
                    let since_ms = if ticket_ms == u64::MAX {
                        *fallback_since.entry(pid).or_insert_with(paths::now_ms)
                    } else {
                        fallback_since.remove(&pid);
                        ticket_ms
                    };
                    let qt = QueueTicket {
                        pos: (idx + 1) as u32,
                        since_ms,
                        repo_busy: is_repo_busy(r, &routers, &slots, &worker_locks),
                    };
                    queue_tickets.insert(format!("agy:{pid}"), qt.clone());
                    queued_by_pid.insert(pid, qt);
                }
                fallback_since.retain(|pid, _| queued_by_pid.contains_key(pid));

                let mut evs = Vec::new();
                let queued = queued_by_pid.len() as u32;
                let (models, role_models) = shared.lock().map(|s| (s.models.clone(), s.role_models.clone())).unwrap_or_default();
                for r in &routers {
                    let id = format!("agy:{}", r.pid);
                    let slot = slots.get(&r.pid).copied();
                    let model = models
                        .get(&r.role)
                        .cloned()
                        .or_else(|| cfg.agy_models.get(&r.role).cloned())
                        .or_else(|| role_models.get(&r.role).cloned());
                    let running = slot.is_some() || r.rss_mb > 0;
                    let qt = queued_by_pid.get(&r.pid);
                    let queue_key = qt.map(|q| (q.pos, q.repo_busy));
                    let prev = live.get(&r.pid);
                    let identity_changed = prev.map(|p| (&p.slot, p.running, &p.model)) != Some((&slot, running, &model));
                    let state_changed = prev.map(|p| (p.slot, p.running, p.queue_key)) != Some((slot, running, queue_key));
                    let who = match &r.domain {
                        Some(d) => format!("{}:{}", r.role, d),
                        None => r.role.clone(),
                    };
                    let job_note = r.rosso_job.as_deref().map(|j| format!(" · job {j}")).unwrap_or_default();
                    if identity_changed {
                        let name = r
                            .rosso_as
                            .clone()
                            .or_else(|| office_core::rosso::title_for(&r.role, r.domain.as_deref()))
                            .unwrap_or_else(|| who.clone());
                        let label = match &model {
                            Some(m) => format!("{}·{}", name, short_model(m)),
                            None => name,
                        };
                        evs.push(EventKind::AgentSeen {
                            id: id.clone(),
                            kind: AgentKind::Agy,
                            label: Some(label),
                            parent: None,
                            cwd: r.cwd.clone(),
                            role: Some(r.role.clone()),
                            model: model.clone(),
                            slot,
                            pid: Some(r.pid),
                            resume_id: None,
                        });
                        if let (Ok(mut s), Some(c)) = (shared.lock(), &r.cwd) {
                            s.recent.insert((r.role.clone(), canon_cwd(c)), id.clone());
                        }
                    }
                    if state_changed {
                        let (state, act) = if let Some(q) = qt {
                            let act = if q.repo_busy {
                                format!("repo busy (#{})", q.pos)
                            } else {
                                format!("queued #{}", q.pos)
                            };
                            (AgentState::Queued, act)
                        } else if !running {
                            (AgentState::Queued, "waiting for a slot".to_string())
                        } else if r.role == "worker" {
                            (AgentState::Typing, format!("editing {}{job_note}", r.cwd.as_deref().map(basename).unwrap_or_default()))
                        } else {
                            (AgentState::Thinking, format!("{} {}{job_note}", who, r.cwd.as_deref().map(basename).unwrap_or_default()))
                        };
                        evs.push(EventKind::State { id: id.clone(), state, activity: Some(act.trim().to_string()) });
                    }
                    // Emit Resources each scan for live routers as heartbeat.
                    evs.push(EventKind::Resources { id: id.clone(), rss_mb: r.rss_mb });
                    live.insert(r.pid, LiveRouter { slot, rss_mb: r.rss_mb, running, model, queue_key });
                }
                let gone: Vec<u32> = live.keys().filter(|p| !routers.iter().any(|r| r.pid == **p)).copied().collect();
                for pid in gone {
                    live.remove(&pid);
                    evs.push(EventKind::Gone { id: format!("agy:{pid}") });
                }
                if queue_tickets != last_tickets {
                    last_tickets = queue_tickets.clone();
                    evs.push(EventKind::QueueTickets(queue_tickets));
                }
                if let Ok(mut s) = shared.lock() {
                    s.queued = queued;
                    if s.recent.len() > 256 {
                        let live_ids: HashSet<String> = live.keys().map(|p| format!("agy:{p}")).collect();
                        s.recent.retain(|_, v| live_ids.contains(v));
                    }
                }
                for k in evs {
                    if tx.send(Event::now(SourceKind::AgyRouter, k)).is_err() {
                        return;
                    }
                }
            }
        }
        tick = tick.wrapping_add(1);
        std::thread::sleep(SCAN_EVERY);
    }
}

// ---------------------------------------------------------------- dry run

pub fn run_dry(route: &str) -> Option<String> {
    let mut child = Command::new(route)
        .args(["--status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let ok = stdout.read_to_string(&mut s).is_ok();
        let _ = tx.send(ok.then_some(s));
    });
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
    rx.recv_timeout(Duration::from_millis(200)).ok().flatten()
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

/// Parse ORPHAN_AGY / RESERVED_MB / WOULD_START / ROLE_MODELS (`role=model/300s role=model/120s`).
pub fn parse_router_extra(text: &str) -> RouterExtra {
    let mut x = RouterExtra::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim() {
            "ORPHAN_AGY" => x.orphans = first_num(v).unwrap_or(0),
            "RESERVED_MB" => x.reserved_mb = first_num(v),
            "WOULD_START" => x.would_start = Some(v.to_ascii_lowercase().starts_with("yes") || v.starts_with("true")),
            "ROLE_MODELS" => {
                for tok in v.split_whitespace() {
                    let Some((role, rest)) = tok.split_once('=') else { continue };
                    let model = rest.split('/').next().unwrap_or("");
                    if !role.is_empty() && !model.is_empty() && x.role_models.len() < 32 {
                        x.role_models.insert(role.to_string(), model.to_string());
                    }
                }
            }
            "RESPONSE" => break,
            _ => {}
        }
    }
    x
}

/// Warn when the orphan count rises above what was last alerted on.
pub fn orphan_alert(orphans: u32, last: u32, now: u64) -> Option<Alert> {
    (orphans > 0 && orphans > last).then(|| Alert {
        at_ms: now,
        level: AlertLevel::Warn,
        agent: None,
        code: "orphan_agy".into(),
        message: format!("{orphans} orphan agy process(es) running without a router"),
    })
}

// ---------------------------------------------------------------- audit log

/// Turn one audit line into an alert (failed calls / needs input). Also returns (role, model).
#[cfg(test)]
pub fn audit_alert(v: &Value, agent: Option<String>, now: u64) -> Option<Alert> {
    audit_alert_hint(v, agent, now, None)
}

/// `audit_alert` with the failure hint ("stalled: retry once") appended when known.
pub fn audit_alert_hint(v: &Value, agent: Option<String>, now: u64, hint: Option<&str>) -> Option<Alert> {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("?");
    let cwd = v.get("cwd").and_then(Value::as_str).map(basename).unwrap_or_default();
    let who = format!("agy {role}{}", if cwd.is_empty() { String::new() } else { format!(" ({cwd})") });
    if let Some(code) = v.get("errorCode").and_then(Value::as_str) {
        let (c, level, what) = match code {
            "TOOL_DENIED" => ("tool_denied", AlertLevel::Warn, "tool call denied"),
            "TIMEOUT" => ("timeout", AlertLevel::Critical, "timed out"),
            "EMPTY_RESPONSE" => ("empty_response", AlertLevel::Warn, "empty response"),
            "BUSY" => ("router_busy", AlertLevel::Warn, "router busy (queued too long)"),
            "PROVIDER_ERROR" => ("provider_error", AlertLevel::Warn, "provider error"),
            "INTERRUPTED" => ("interrupted", AlertLevel::Warn, "interrupted"),
            "BLOCKED" => ("blocked", AlertLevel::Warn, "blocked"),
            _ => ("agy_error", AlertLevel::Warn, code),
        };
        let message = match hint.filter(|h| !h.is_empty()) {
            Some(h) => format!("{who}: {what} ({h})"),
            None => format!("{who}: {what}"),
        };
        return Some(Alert { at_ms: now, level, agent, code: c.into(), message });
    }
    if v.get("needsInput").and_then(Value::as_bool) == Some(true) {
        return Some(Alert { at_ms: now, level: AlertLevel::Warn, agent, code: "needs_you".into(), message: format!("{who}: NEEDS_USER_INPUT") });
    }
    None
}

/// Idle seconds from which a timeout counts as a stall (no activity) rather than a too-big task.
const STALL_IDLE_S: f64 = 90.0;

/// Diagnose a failed call from the audit line and (optionally) its failure file.
/// Returns None when the call did not fail.
pub fn failure_info(audit: &Value, file: Option<&Value>, now: u64) -> Option<FailureInfo> {
    let code = audit.get("errorCode").and_then(Value::as_str).unwrap_or("");
    let has_file = audit.get("failureFile").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if code.is_empty() && !has_file && file.is_none() {
        return None;
    }
    let outcome = file
        .and_then(|f| f.get("outcome").and_then(Value::as_str))
        .filter(|o| !o.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| if code.is_empty() { "ERROR".into() } else { code.to_string() });
    let timeout = outcome.contains("TIMEOUT") || code == "TIMEOUT";
    let explicit = file.and_then(|f| f.get("TIMEOUT_DIAGNOSIS")).and_then(|d| match d {
        Value::String(s) => Some(s.to_ascii_uppercase()),
        Value::Object(o) => o.values().find_map(|x| x.as_str().map(str::to_ascii_uppercase)),
        _ => None,
    });
    let idle = file.and_then(|f| f.get("idleS")).or_else(|| audit.get("idleS")).and_then(Value::as_f64);
    let diagnosis = if let Some(d) = explicit.filter(|d| d.contains("STALL") || d.contains("ACTIVE") || d.contains("BIG")) {
        if d.contains("STALL") { "STALLED" } else { "ACTIVE_TOO_BIG" }.to_string()
    } else if timeout {
        match idle {
            Some(i) if i >= STALL_IDLE_S => "STALLED".to_string(),
            _ => "ACTIVE_TOO_BIG".to_string(),
        }
    } else {
        String::new()
    };
    let hint = match diagnosis.as_str() {
        "STALLED" => "stalled: retry once",
        "ACTIVE_TOO_BIG" => "too big: split the task",
        _ => "failed: see the run log",
    };
    Some(FailureInfo { at_ms: now, outcome, diagnosis, hint: hint.to_string() })
}

/// Read the failure file named by an audit line (bounded, JSON only).
fn read_failure_file(audit: &Value) -> Option<Value> {
    let p = audit.get("failureFile").and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let f = std::fs::File::open(p).ok()?;
    let mut buf = Vec::new();
    f.take(512 * 1024).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

/// Verdict, failure and alert events for one audit line. `agent` is the agent the call belongs to
/// (Verdict/Failure need it; the alert does not).
pub fn audit_events(v: &Value, agent: Option<String>, now: u64, failure_file: Option<&Value>) -> Vec<EventKind> {
    let mut out = Vec::new();
    if let (Some(id), Some(verdict)) = (agent.clone(), v.get("verdict").and_then(Value::as_str).filter(|s| !s.is_empty())) {
        out.push(EventKind::Verdict { id, verdict: verdict.to_string() });
    }
    let fail = failure_info(v, failure_file, now);
    if let (Some(id), Some(info)) = (agent.clone(), fail.clone()) {
        out.push(EventKind::Failure { id, info });
    }
    let hint = fail.as_ref().map(|f| f.hint.as_str());
    match audit_alert_hint(v, agent.clone(), now, hint) {
        Some(a) => out.push(EventKind::Alert(a)),
        None => {
            if let Some(f) = &fail {
                let role = v.get("role").and_then(Value::as_str).unwrap_or("?");
                out.push(EventKind::Alert(Alert {
                    at_ms: now,
                    level: AlertLevel::Warn,
                    agent,
                    code: "agy_failure".into(),
                    message: format!("agy {role}: call failed ({})", f.hint),
                }));
            }
        }
    }
    out
}

/// A finished agy call's result, handed back to the relay that started it. The full response
/// text is included when the router saved it (long answers); otherwise the relay's own report
/// (which quotes the router block verbatim) carries it.
fn audit_report(v: &Value, from: String) -> EventKind {
    let g = |k: &str| v.get(k).map(|x| match x {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }).unwrap_or_default();
    let status = g("status");
    let code = g("errorCode");
    let secs = v.get("durationMs").and_then(Value::as_u64).unwrap_or(0) / 1000;
    let mut body = format!(
        "STATUS: {status}{}\nROLE: {}\nMODEL: {}\nDURATION: {secs} s (queued {} ms)\nRESPONSE: {} chars",
        if code.is_empty() { String::new() } else { format!(" ({code})") },
        g("role"),
        g("model"),
        g("queuedMs"),
        g("responseChars"),
    );
    let file = g("fullResponseFile");
    if !file.is_empty() {
        if let Ok(text) = std::fs::read_to_string(&file) {
            body.push_str("\n\n");
            body.push_str(&text.chars().take(15_000).collect::<String>());
        }
    }
    let title = format!("{} {}{}", g("role"), status, if code.is_empty() { String::new() } else { format!(" {code}") });
    EventKind::Handoff { from, to: None, kind: ReportKind::Report, title, body, brief: String::new() }
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
                            agent = s.recent.get(&(role, canon_cwd(&cwd))).cloned();
                        }
                        if replay {
                            if let Some(from) = agent.clone() {
                                if tx.send(Event::now(SourceKind::AgyRouter, audit_report(&v, from))).is_err() {
                                    return;
                                }
                            }
                            let ff = read_failure_file(&v);
                            for k in audit_events(&v, agent, paths::now_ms(), ff.as_ref()) {
                                if tx.send(Event::now(SourceKind::AgyRouter, k)).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                } else if partial.len() > (8 << 20) {
                    partial.clear();
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
  201     1  40000 /opt/homebrew/bin/bun /Users/u/.claude/bin/agy-route --role worker --status
  300     1   1000 /bin/zsh -c grep agy-route --role
";
        let r = find_routers(&parse_ps(ps));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], RouterProc { pid: 100, role: "worker".into(), domain: None, cwd: Some("/p/repo".into()), rss_mb: 300, rosso_job: None, rosso_as: None });
        let mgr = "  500 1 1000 /opt/homebrew/bin/bun /Users/u/.claude/bin/agy-route --role manager --domain frontend --cwd /p/repo\n";
        let m = find_routers(&parse_ps(mgr));
        assert_eq!(m[0].domain.as_deref(), Some("frontend"));
        let ro = "  600 1 1000 /opt/homebrew/bin/bun agy-route --role council-advisor --rosso-job demo --as Security Seat --cwd /p/repo --prompt-file /tmp/x\n";
        let rr = find_routers(&parse_ps(ro));
        assert_eq!(rr[0].rosso_job.as_deref(), Some("demo"));
        assert_eq!(rr[0].rosso_as.as_deref(), Some("Security Seat"));
        assert_eq!(rr[0].cwd.as_deref(), Some("/p/repo"));
        let spaced = "  400 1 1000 /opt/homebrew/bin/bun agy-route --role worker --cwd /Users/u/My App --prompt-file /tmp/x\n";
        let rs = find_routers(&parse_ps(spaced));
        assert_eq!(rs[0].cwd.as_deref(), Some("/Users/u/My App"));
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
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agy-route.slot2.lock"), "4242").unwrap();
        std::fs::write(d.join("agy-route.slot3.lock"), "5150 5140 1700000000\n{\"role\":\"worker\"}").unwrap();
        std::fs::write(d.join("agy-route.worker-abc.lock"), "4242").unwrap();
        let m = read_slots(&d);
        assert_eq!(m.get(&4242), Some(&2));
        assert_eq!(m.get(&5150), Some(&3));
        assert_eq!(m.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_queue_tickets_fifo_and_worker_locks() {
        let d = std::env::temp_dir().join(format!("office-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("queue")).unwrap();
        // Valid tickets out of order, plus tie-break on pid, plus invalid filenames
        for name in [
            "1700000000200-30",
            "1700000000100-50",
            "1700000000100-20",
            "123456789012-99",   // 12 digits (< 13) -> ignored
            "1700000000100-abc", // non-digit pid -> ignored
            "not-a-ticket",
            "1700000000100-+10", // '+' sign -> ignored
        ] {
            std::fs::write(d.join("queue").join(name), "").unwrap();
        }
        assert!(has_any_lock(&d));
        assert_eq!(
            read_queue_tickets(&d),
            vec![
                (1_700_000_000_100, 20),
                (1_700_000_000_100, 50),
                (1_700_000_000_200, 30),
            ]
        );

        std::fs::write(d.join("agy-route.worker-repo1.lock"), "701 700 12345\n{}").unwrap();
        std::fs::write(d.join("agy-route.worker-repo2.lock"), "702").unwrap();
        std::fs::write(d.join("agy-route.slot0.lock"), "999").unwrap();
        let locks = read_worker_locks(&d);
        assert_eq!(locks, HashSet::from([701, 702]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detects_repo_busy_for_workers() {
        let proc = |pid: u32, role: &str, cwd: Option<&str>, rss_mb: u32| RouterProc {
            pid,
            role: role.into(),
            domain: None,
            cwd: cwd.map(str::to_string),
            rss_mb,
            rosso_job: None,
            rosso_as: None,
        };
        let queued_worker = proc(10, "worker", Some("/p/repo-a"), 0);
        let running_same_repo = proc(11, "worker", Some("/p/repo-a"), 120);
        let running_other_repo = proc(12, "worker", Some("/p/repo-b"), 120);
        let queued_advisor = proc(13, "council-advisor", Some("/p/repo-a"), 0);

        let slots = HashMap::new();
        let locks = HashSet::new();

        // Same-repo running worker marks queued worker as busy, but not non-worker
        let routers = vec![queued_worker.clone(), running_same_repo.clone(), queued_advisor.clone()];
        assert!(is_repo_busy(&queued_worker, &routers, &slots, &locks));
        assert!(!is_repo_busy(&queued_advisor, &routers, &slots, &locks));

        // Different-repo running worker does not mark queued worker as busy
        let routers_diff = vec![queued_worker.clone(), running_other_repo.clone()];
        assert!(!is_repo_busy(&queued_worker, &routers_diff, &slots, &locks));

        // Another live worker holding a worker lock marks unslotted worker as busy
        let other_worker_idle_rss = proc(14, "worker", Some("/p/repo-c"), 0);
        let locks_other = HashSet::from([14]);
        let routers_locked = vec![queued_worker.clone(), other_worker_idle_rss];
        assert!(is_repo_busy(&queued_worker, &routers_locked, &slots, &locks_other));

        // If the worker itself holds a slot and no same-repo worker is running, worker lock alone doesn't trigger
        let slots_self = HashMap::from([(10, 0)]);
        assert!(!is_repo_busy(&queued_worker, &routers_locked, &slots_self, &locks_other));
    }

    #[test]
    fn status_extras_parse_and_orphans_alert() {
        let t = "STATUS: OK\nRESERVED_MB: 640\nORPHAN_AGY: 2\nWOULD_START: yes\nROLE_MODELS: architect=gemini-4-neon-high/360s worker=gemini-4-hydrogen-low/540s odd\nDOMAINS: a\n";
        let x = parse_router_extra(t);
        assert_eq!((x.orphans, x.reserved_mb, x.would_start), (2, Some(640), Some(true)));
        assert_eq!(x.role_models.get("worker").map(String::as_str), Some("gemini-4-hydrogen-low"));
        assert_eq!(x.role_models.len(), 2);
        assert!(parse_router_extra("WOULD_START: no (low RAM)\n").would_start == Some(false));
        let a = orphan_alert(2, 0, 9).unwrap();
        assert_eq!((a.code.as_str(), a.level), ("orphan_agy", AlertLevel::Warn));
        assert!(orphan_alert(2, 2, 9).is_none());
        assert!(orphan_alert(0, 1, 9).is_none());
        // a real status block parses without clobbering the plain router fields
        assert_eq!(parse_dry_run("CAPACITY: 3\nORPHAN_AGY: 1\n").capacity, Some(3));
    }

    #[test]
    fn provider_errors_and_failures_get_alerts_hints_and_verdicts() {
        for (code, expect) in [("PROVIDER_ERROR", "provider_error"), ("INTERRUPTED", "interrupted"), ("BLOCKED", "blocked")] {
            let v: Value = serde_json::from_str(&format!(r#"{{"role":"worker","cwd":"/p/X","errorCode":"{code}"}}"#)).unwrap();
            assert_eq!(audit_alert(&v, None, 1).unwrap().code, expect);
        }
        let audit: Value = serde_json::from_str(r#"{"role":"tester","cwd":"/p/X","errorCode":"TIMEOUT","failureFile":"/nonexistent/f.json","verdict":"FAIL"}"#).unwrap();
        let stalled: Value = serde_json::from_str(r#"{"outcome":"ERROR/TIMEOUT","idleS":240}"#).unwrap();
        let big: Value = serde_json::from_str(r#"{"outcome":"ERROR/TIMEOUT","idleS":3}"#).unwrap();
        let explicit: Value = serde_json::from_str(r#"{"outcome":"TIMEOUT","TIMEOUT_DIAGNOSIS":"ACTIVE_TOO_BIG","idleS":900}"#).unwrap();
        assert_eq!(failure_info(&audit, Some(&stalled), 1).unwrap().hint, "stalled: retry once");
        assert_eq!(failure_info(&audit, Some(&big), 1).unwrap().diagnosis, "ACTIVE_TOO_BIG");
        assert_eq!(failure_info(&audit, Some(&explicit), 1).unwrap().hint, "too big: split the task");
        let ok: Value = serde_json::from_str(r#"{"role":"tester","errorCode":null,"verdict":"PASS"}"#).unwrap();
        assert!(failure_info(&ok, None, 1).is_none());
        let evs = audit_events(&audit, Some("agy:7".into()), 5, Some(&stalled));
        assert!(matches!(&evs[0], EventKind::Verdict { id, verdict } if id == "agy:7" && verdict == "FAIL"));
        assert!(matches!(&evs[1], EventKind::Failure { info, .. } if info.diagnosis == "STALLED"));
        match &evs[2] {
            EventKind::Alert(a) => {
                assert_eq!(a.code, "timeout");
                assert!(a.message.contains("stalled: retry once"));
            }
            k => panic!("{k:?}"),
        }
        // a PASS with no failure: verdict only
        let evs = audit_events(&ok, Some("agy:7".into()), 5, None);
        assert_eq!(evs.len(), 1);
        // verdict without an attributed agent is dropped
        assert!(audit_events(&ok, None, 5, None).is_empty());
    }
}
