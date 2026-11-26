//! Unix socket listener for office-hook lines; decodes Claude Code and Antigravity hook payloads into Events.
use super::{basename, reports, short};
use office_core::model::{AgentKind, AgentState, ReportKind};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-line cap (office-hook caps stdin at 1 MB; leave room for the envelope).
const MAX_LINE: u64 = (1 << 20) + 64 * 1024;
const CONN_WORKERS: usize = 4;
const CONN_QUEUE: usize = 64;
const CONN_DEADLINE: Duration = Duration::from_secs(5);

/// Removes the unix socket file (and our pid file) when the office exits cleanly.
pub struct SocketGuard(pub PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let pf = pid_file(&self.0);
        if read_pid(&pf) == Some(std::process::id()) {
            let _ = std::fs::remove_file(&pf);
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// `office.pid` next to the socket: the pid of the office that owns the hook socket.
pub fn pid_file(sock: &Path) -> PathBuf {
    sock.with_file_name("office.pid")
}

fn read_pid(p: &Path) -> Option<u32> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    pid > 1 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// True when `pid` is an `office` TUI process (never office-hook, never anything else).
fn is_office_process(pid: u32) -> bool {
    std::process::Command::new("/bin/ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().rsplit('/').next().unwrap_or("").to_string())
        .is_some_and(|c| c == "office")
}

/// Pids of `office` processes holding the socket: from the pid file, else from `lsof`
/// (older builds wrote no pid file).
fn socket_holders(sock: &Path) -> Vec<u32> {
    let me = std::process::id();
    let mut pids: Vec<u32> = read_pid(&pid_file(sock)).into_iter().collect();
    if pids.is_empty() {
        if let Ok(o) = std::process::Command::new("/usr/sbin/lsof").arg("-t").arg(sock).output() {
            pids = String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect();
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids.into_iter().filter(|&p| p != me && pid_alive(p) && is_office_process(p)).collect()
}

/// The newest office wins: ask older office instances holding the socket to quit (SIGTERM,
/// then SIGKILL after ~1.5 s for orphans that no longer respond). Returns the pids asked.
fn take_over(sock: &Path) -> Vec<u32> {
    let holders = socket_holders(sock);
    for &p in &holders {
        unsafe { libc::kill(p as libc::pid_t, libc::SIGTERM) };
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline && holders.iter().any(|&p| pid_alive(p)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for &p in &holders {
        if pid_alive(p) && is_office_process(p) {
            unsafe { libc::kill(p as libc::pid_t, libc::SIGKILL) };
        }
    }
    holders
}

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<SocketGuard> {
    let sock = paths::hook_socket();
    let listener = bind(&sock)?;
    std::thread::Builder::new().name("hook-socket".into()).spawn(move || serve(listener, tx))?;
    Ok(SocketGuard(sock))
}

/// Bind the socket: create its dir (0700 when it's our runtime dir), drop a stale socket file.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        if dir == paths::runtime_dir() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    if std::fs::symlink_metadata(path).is_ok() {
        if UnixStream::connect(path).is_ok() {
            // Another office (often an orphan left behind by a closed terminal window) owns the
            // socket, so this one would get no hook events. Take it over.
            take_over(path);
            std::thread::sleep(Duration::from_millis(100));
            if UnixStream::connect(path).is_ok() {
                anyhow::bail!("another office is already listening on {} and did not hand over", path.display());
            }
        }
        let _ = std::fs::remove_file(path);
    }
    let l = UnixListener::bind(path)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    let _ = std::fs::write(pid_file(path), std::process::id().to_string());
    Ok(l)
}

fn handle_conn(stream: UnixStream, tx: &Sender<Event>) -> bool {
    let deadline = Instant::now() + CONN_DEADLINE;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut r = BufReader::new(stream.take(MAX_LINE * 4));
    let mut line = String::new();
    loop {
        if Instant::now() >= deadline {
            break;
        }
        line.clear();
        match (&mut r).take(MAX_LINE).read_line(&mut line) {
            Ok(0) => break,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                if !line.is_empty() && Instant::now() < deadline {
                    continue;
                }
                break;
            }
            Err(_) => break,
            Ok(_) => {
                for ev in decode_line(line.trim(), paths::now_ms()) {
                    if tx.send(ev).is_err() {
                        return false;
                    }
                }
            }
        }
    }
    true
}

pub fn serve(listener: UnixListener, tx: Sender<Event>) {
    let (conn_tx, conn_rx) = mpsc::sync_channel::<UnixStream>(CONN_QUEUE);
    let conn_rx: Arc<Mutex<Receiver<UnixStream>>> = Arc::new(Mutex::new(conn_rx));
    for i in 0..CONN_WORKERS {
        let rx = conn_rx.clone();
        let tx = tx.clone();
        let _ = std::thread::Builder::new().name(format!("hook-worker-{i}")).spawn(move || loop {
            let stream = {
                let Ok(guard) = rx.lock() else { return };
                guard.recv()
            };
            let Ok(stream) = stream else { return };
            if !handle_conn(stream, &tx) {
                return;
            }
        });
    }
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        if conn_tx.try_send(stream).is_err() {
            // Queue saturated: drop slow extra connection rather than exhausting OS threads.
        }
    }
}

fn s<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty())
}

/// Short human summary of a tool call: file basename, first 40 chars of a command, pattern, url…
pub fn tool_summary(tool: &str, input: &Value) -> String {
    if matches!(tool, "Grep" | "Glob" | "grep_search" | "find_by_name") {
        let pat = s(input, &["pattern", "Query", "query"]);
        let path = s(input, &["path", "Path", "SearchPath", "DirectoryPath"]);
        match (pat, path) {
            (Some(p), Some(dir)) => return short(&format!("{p} @ {}", basename(dir)), 40),
            (Some(p), None) => return short(p, 40),
            _ => {}
        }
    }
    if matches!(tool, "Agent" | "Task" | "invoke_subagent") {
        let role = s(input, &["subagent_type", "Role"]);
        let desc = s(input, &["description", "prompt"]);
        match (role, desc) {
            (Some(r), Some(d)) => return short(&format!("{r}: {d}"), 40),
            (Some(r), None) => return short(r, 40),
            (None, Some(d)) => return short(d, 40),
            (None, None) => {}
        }
    }
    if matches!(tool, "Bash" | "run_command") {
        if let Some(d) = s(input, &["description"]) {
            return short(d, 40);
        }
        if let Some(c) = s(input, &["command", "CommandLine"]) {
            return short(c, 40);
        }
    }
    if let Some((srv, t)) = tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
        return short(&format!("{srv}:{t}"), 40);
    }
    if let Some(p) = s(
        input,
        &["file_path", "notebook_path", "path", "Path", "TargetFile", "AbsolutePath", "DirectoryPath", "SearchPath"],
    ) {
        return short(&basename(p), 40);
    }
    if let Some(c) = s(input, &["command", "CommandLine"]) {
        return short(c, 40);
    }
    if tool == "AskUserQuestion" {
        if let Some(q) = input.get("questions").and_then(|q| q.get(0)).and_then(|q| q.get("question")).and_then(Value::as_str) {
            return short(q, 40);
        }
    }
    if let Some(x) = s(input, &["pattern", "url", "query", "Query", "description", "prompt"]) {
        return short(x, 40);
    }
    String::new()
}

/// Decode one office-hook line into Events (empty on garbage).
pub fn decode_line(line: &str, now: u64) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
    let at = v.get("ts").and_then(Value::as_u64).unwrap_or(now);
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let hook_event = s(&v, &["hook_event"]).map(str::to_string);
    let request_id = s(&v, &["request_id"]).map(str::to_string);
    match s(&v, &["source"]).unwrap_or("claude") {
        "antigravity" | "agy" | "gemini" => decode_antigravity(&payload, hook_event.as_deref(), request_id, at),
        _ => decode_claude(&payload, hook_event.as_deref(), request_id, at),
    }
}

fn ev(at: u64, source: SourceKind, kind: EventKind) -> Event {
    Event { at_ms: at, source, kind }
}

fn decode_claude(p: &Value, hook_event: Option<&str>, request_id: Option<String>, at: u64) -> Vec<Event> {
    let Some(session) = s(p, &["session_id"]) else { return vec![] };
    let name = s(p, &["hook_event_name"]).or(hook_event).unwrap_or("");
    let cwd = s(p, &["cwd"]).map(str::to_string);
    let agent_id = s(p, &["agent_id"]);
    let agent_type = s(p, &["agent_type"]).map(str::to_string);
    let src = SourceKind::ClaudeHook;
    let parent_id = format!("cc:{session}");
    let mut out = Vec::new();

    let id = match agent_id {
        Some(aid) => {
            let id = format!("cc-sub:{session}/{aid}");
            out.push(ev(
                at,
                src,
                EventKind::AgentSeen {
                    id: id.clone(),
                    kind: AgentKind::ClaudeSub,
                    label: Some(
                        agent_type
                            .as_deref()
                            .and_then(|t| office_core::rosso::title_for(t, None))
                            .or_else(|| agent_type.clone())
                            .unwrap_or_else(|| "sub".into()),
                    ),
                    parent: Some(parent_id.clone()),
                    cwd: cwd.clone(),
                    role: agent_type.clone(),
                    model: None,
                    slot: None,
                    pid: None,
                    resume_id: None,
                },
            ));
            id
        }
        None => {
            out.push(ev(
                at,
                src,
                EventKind::AgentSeen {
                    id: parent_id.clone(),
                    kind: AgentKind::Claude,
                    label: cwd.as_deref().map(|c| format!("cc·{}", basename(c))),
                    parent: None,
                    cwd: cwd.clone(),
                    role: agent_type.clone(),
                    model: None,
                    slot: None,
                    pid: None,
                    resume_id: Some(session.to_string()),
                },
            ));
            parent_id.clone()
        }
    };
    let tool = s(p, &["tool_name"]).unwrap_or("").to_string();
    let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
    let state = |st: AgentState, act: Option<String>| EventKind::State { id: id.clone(), state: st, activity: act };
    match name {
        "SessionStart" => {}
        "SubagentStart" => {
            out.push(ev(at, src, state(AgentState::Thinking, Some("starting…".into()))));
        }
        "UserPromptSubmit" => {
            let prompt = s(p, &["prompt"]).map(|x| short(x, 40));
            out.push(ev(at, src, state(AgentState::Thinking, prompt)));
        }
        "PreToolUse" => {
            out.push(ev(at, src, EventKind::ToolStart { id: id.clone(), tool: tool.clone(), summary: tool_summary(&tool, &input) }));
        }
        "PostToolUse" => out.push(ev(at, src, EventKind::ToolEnd { id: id.clone(), tool, ok: true })),
        "PostToolUseFailure" => {
            let interrupted = p.get("is_interrupt").and_then(Value::as_bool).unwrap_or(false);
            out.push(ev(at, src, EventKind::ToolEnd { id: id.clone(), tool, ok: interrupted }));
        }
        "Notification" => {
            let ntype = s(p, &["notification_type"]).unwrap_or("");
            let msg = s(p, &["message"]).unwrap_or("").to_string();
            let needs = matches!(ntype, "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input")
                || (ntype.is_empty() && msg.to_ascii_lowercase().contains("permission"));
            if needs {
                out.push(ev(at, src, state(AgentState::NeedsYou, Some(short(&msg, 60)))));
            } else if ntype == "idle_prompt" {
                out.push(ev(at, src, state(AgentState::Idle, Some(String::new()))));
            }
        }
        "PermissionRequest" => {
            let request_id = request_id.unwrap_or_else(|| paths::permission_request_id(p));
            let summary = format!("{tool} {}", tool_summary(&tool, &input)).trim().to_string();
            out.push(ev(at, src, EventKind::PermissionRequested { id: id.clone(), request_id, summary }));
        }
        "OfficePermissionResolved" => {
            if let Some(r) = request_id.or_else(|| s(p, &["request_id"]).map(str::to_string)) {
                out.push(ev(at, src, EventKind::PermissionResolved { id: id.clone(), request_id: r }));
            }
        }
        "Stop" => {
            out.push(ev(at, src, state(AgentState::Idle, Some(String::new()))));
            // A top-level session finished a turn: its answer goes to you (the boss).
            if agent_id.is_none() {
                let body = s(p, &["last_assistant_message"])
                    .map(str::to_string)
                    .or_else(|| s(p, &["transcript_path"]).and_then(|t| reports::last_report(Path::new(t))));
                if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
                    out.push(ev(at, src, EventKind::Handoff {
                        from: id.clone(),
                        to: None,
                        kind: ReportKind::Result,
                        title: reports::title_of(&body, 60),
                        body,
                        brief: String::new(),
                    }));
                }
            }
        }
        "StopFailure" => out.push(ev(at, src, state(AgentState::Error, s(p, &["error", "message"]).map(|x| short(x, 40))))),
        "SubagentStop" => {
            out.push(ev(at, src, state(AgentState::Done, None)));
            // The subagent's report travels back to the session that briefed it.
            let path = s(p, &["agent_transcript_path"]).map(PathBuf::from).or_else(|| {
                let t = s(p, &["transcript_path"])?;
                Some(reports::subagent_transcript(Path::new(t), agent_id?))
            });
            let body = s(p, &["last_assistant_message"])
                .map(str::to_string)
                .or_else(|| path.as_deref().and_then(reports::last_report))
                .unwrap_or_default();
            let brief = path.as_deref().and_then(reports::first_brief).unwrap_or_default();
            let title = path
                .as_deref()
                .and_then(reports::subagent_description)
                .unwrap_or_else(|| if body.is_empty() { "finished".into() } else { reports::title_of(&body, 60) });
            out.push(ev(at, src, EventKind::Handoff { from: id.clone(), to: Some(parent_id.clone()), kind: ReportKind::Report, title, body, brief }));
        }
        "SessionEnd" => out.push(ev(at, src, EventKind::Gone { id: id.clone() })),
        _ => {}
    }
    out
}

fn cwd_hash6(c: &str) -> u32 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in c.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & 0x00ff_ffff) as u32
}

/// Antigravity CLI hook payloads: protojson camelCase + snake_case variants.
fn decode_antigravity(p: &Value, hook_event: Option<&str>, _request_id: Option<String>, at: u64) -> Vec<Event> {
    let name = hook_event.or_else(|| s(p, &["hook_event_name", "hookEventName", "event"])).unwrap_or("");
    let cwd = s(p, &["cwd", "workspace", "workspaceRoot"])
        .or_else(|| {
            p.get("workspacePaths")
                .and_then(Value::as_array)
                .and_then(|arr| arr.iter().filter_map(Value::as_str).find(|x| !x.is_empty()))
        })
        .map(str::to_string);
    let raw_sid = s(p, &["conversationId", "conversation_id", "sessionId", "session_id"]);
    let sid = raw_sid
        .map(str::to_string)
        .or_else(|| cwd.clone().map(|c| format!("cwd-{}-{:06x}", paths::sanitize_id(&basename(&c)), cwd_hash6(&c))));
    let Some(sid) = sid else { return vec![] };
    let resume_id = raw_sid.map(str::to_string);
    let model = s(p, &["modelName", "model"]).filter(|m| *m != "auto").map(str::to_string);
    let id = format!("ag:{sid}");
    let src = SourceKind::AgyHook;
    let mut out = vec![ev(
        at,
        src,
        EventKind::AgentSeen {
            id: id.clone(),
            kind: AgentKind::Other,
            label: Some(format!("ag·{}", cwd.as_deref().map(basename).unwrap_or_else(|| "agy".into()))),
            parent: None,
            cwd,
            role: None,
            model,
            slot: None,
            pid: p.get("pid").and_then(Value::as_u64).map(|x| x as u32),
            resume_id,
        },
    )];
    let tool_call = p.get("toolCall");
    let tool = tool_call
        .and_then(|tc| s(tc, &["name"]))
        .or_else(|| s(p, &["tool_name", "toolName", "tool"]))
        .unwrap_or("")
        .to_string();
    let input = tool_call
        .and_then(|tc| tc.get("args"))
        .or_else(|| p.get("tool_input"))
        .or_else(|| p.get("toolInput"))
        .or_else(|| p.get("args"))
        .cloned()
        .unwrap_or(Value::Null);
    match name {
        "PreToolUse" | "BeforeTool" => {
            out.push(ev(at, src, EventKind::ToolStart { id: id.clone(), tool: tool.clone(), summary: tool_summary(&tool, &input) }));
        }
        "PostToolUse" | "AfterTool" => {
            let ok = match p.get("error") {
                None | Some(Value::Null) | Some(Value::Bool(false)) => true,
                Some(Value::String(st)) => st.trim().is_empty(),
                _ => false,
            };
            out.push(ev(at, src, EventKind::ToolEnd { id: id.clone(), tool, ok }));
        }
        "UserPromptSubmit" | "PreInvocation" | "BeforeAgent" => {
            let activity = s(p, &["lastUserInput", "prompt"]).map(|x| short(x, 40));
            out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Thinking, activity }));
        }
        "PostInvocation" => {
            out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Thinking, activity: None }));
        }
        "Stop" | "AfterAgent" => {
            let err_str = s(p, &["error"]).filter(|e| !e.trim().is_empty());
            let term = s(p, &["terminationReason"]).unwrap_or("");
            if err_str.is_some() || term == "ERROR" {
                let activity = err_str.or((!term.is_empty()).then_some(term)).map(|x| short(x, 40));
                out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Error, activity }));
            } else {
                out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Idle, activity: Some(String::new()) }));
            }
            let tp = s(p, &["transcriptPath", "transcript_path"]);
            if let Some(tp_str) = tp {
                if let Some((ti, cr, to, um)) = reports::agy_usage(Path::new(tp_str)) {
                    out.push(ev(
                        at,
                        src,
                        EventKind::Usage {
                            id: id.clone(),
                            tokens_in: ti,
                            tokens_out: to,
                            tokens_cache_read: cr,
                            tokens_cache_write: 0,
                            cost_usd: 0.0,
                            context_pct: None,
                            model: um,
                        },
                    ));
                }
            }
            let body = s(p, &["last_assistant_message", "lastAssistantMessage", "response"])
                .filter(|b| !b.trim().is_empty())
                .map(str::to_string)
                .or_else(|| tp.and_then(|t| reports::last_agy_report(Path::new(t))));
            if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
                out.push(ev(
                    at,
                    src,
                    EventKind::Handoff {
                        from: id,
                        to: None,
                        kind: ReportKind::Result,
                        title: reports::title_of(&body, 60),
                        body,
                        brief: s(p, &["lastUserInput", "prompt"]).unwrap_or("").to_string(),
                    },
                ));
            }
        }
        "SessionEnd" => out.push(ev(at, src, EventKind::Gone { id })),
        _ => {}
    }
    out
}

/// Build office-hook once per test binary (avoids parallel cargo build lock contention) and return its path.
#[cfg(test)]
fn office_hook_bin() -> &'static Path {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let out = std::process::Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "office-hook", "--message-format=json"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("cargo build office-hook");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["target"]["name"] == "office-hook")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("office-hook executable path")
    })
    .as_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter().map(|e| format!("{:?}", e.kind).split([' ', '{', '(']).next().unwrap().to_string()).collect()
    }

    #[test]
    fn decodes_pre_tool_use() {
        let line = r#"{"source":"claude","ts":5,"payload":{"session_id":"s1","cwd":"/a/DICE","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test -p office-core --all-features --verbose please"}}}"#;
        let evs = decode_line(line, 0);
        assert_eq!(kinds(&evs), vec!["AgentSeen", "ToolStart"]);
        match &evs[0].kind {
            EventKind::AgentSeen { id, label, resume_id, .. } => {
                assert_eq!(id, "cc:s1");
                assert_eq!(label.as_deref(), Some("cc·DICE"));
                assert_eq!(resume_id.as_deref(), Some("s1"));
            }
            _ => unreachable!(),
        }
        match &evs[1].kind {
            EventKind::ToolStart { tool, summary, .. } => {
                assert_eq!(tool, "Bash");
                assert!(summary.starts_with("cargo test -p office-core") && summary.ends_with('…'));
            }
            _ => unreachable!(),
        }
        assert_eq!(evs[1].at_ms, 5);
    }

    #[test]
    fn decodes_subagent_and_edit_summary() {
        let line = r#"{"source":"claude","payload":{"session_id":"s1","agent_id":"a9","agent_type":"Explore","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"/x/y/main.rs"}}}"#;
        let evs = decode_line(line, 1);
        match &evs[0].kind {
            EventKind::AgentSeen { id, kind, parent, role, .. } => {
                assert_eq!(id, "cc-sub:s1/a9");
                assert_eq!(*kind, AgentKind::ClaudeSub);
                assert_eq!(parent.as_deref(), Some("cc:s1"));
                assert_eq!(role.as_deref(), Some("Explore"));
            }
            _ => unreachable!(),
        }
        assert!(matches!(&evs[1].kind, EventKind::ToolStart { summary, .. } if summary == "main.rs"));
        let stop = r#"{"source":"claude","payload":{"session_id":"s1","agent_id":"a9","hook_event_name":"SubagentStop"}}"#;
        assert_eq!(kinds(&decode_line(stop, 1)), vec!["AgentSeen", "State", "Handoff"]);
    }

    #[test]
    fn decodes_permission_notification_and_end() {
        let n = r#"{"source":"claude","payload":{"session_id":"s","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}}"#;
        assert!(matches!(decode_line(n, 0)[1].kind, EventKind::State { state: AgentState::NeedsYou, .. }));
        let pr = r#"{"source":"claude","request_id":"rq","payload":{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf x"}}}"#;
        assert!(matches!(&decode_line(pr, 0)[1].kind, EventKind::PermissionRequested { request_id, summary, .. } if request_id == "rq" && summary == "Bash rm -rf x"));
        let end = r#"{"source":"claude","payload":{"session_id":"s","hook_event_name":"SessionEnd"}}"#;
        assert!(matches!(decode_line(end, 0)[1].kind, EventKind::Gone { .. }));
        assert!(decode_line("not json", 0).is_empty());
    }

    #[test]
    fn decodes_antigravity() {
        let l = r#"{"source":"antigravity","hook_event":"PreToolUse","payload":{"cwd":"/p/agent-office","tool_name":"Read","tool_input":{"path":"/p/a.rs"}}}"#;
        let evs = decode_line(l, 0);
        assert!(matches!(&evs[0].kind, EventKind::AgentSeen { kind: AgentKind::Other, label: Some(l), .. } if l == "ag·agent-office"));
        assert!(matches!(&evs[1].kind, EventKind::ToolStart { summary, .. } if summary == "a.rs"));
    }

    #[test]
    fn decodes_antigravity_camel_case_events() {
        // PreInvocation with conversationId, workspacePaths, lastUserInput, modelName
        let pre_inv = r#"{"source":"antigravity","hook_event":"PreInvocation","payload":{"conversationId":"conv-123","workspacePaths":["/work/my-proj"],"lastUserInput":"fix the login bug","modelName":"gemini-4-neon-high"}}"#;
        let evs = decode_line(pre_inv, 10);
        assert_eq!(kinds(&evs), vec!["AgentSeen", "State"]);
        assert!(matches!(
            &evs[0].kind,
            EventKind::AgentSeen { id, cwd: Some(cwd), label: Some(label), model: Some(model), resume_id: Some(rid), .. }
            if id == "ag:conv-123" && cwd == "/work/my-proj" && label == "ag·my-proj" && model == "gemini-4-neon-high" && rid == "conv-123"
        ));
        assert!(matches!(
            &evs[1].kind,
            EventKind::State { id, state: AgentState::Thinking, activity: Some(act) }
            if id == "ag:conv-123" && act == "fix the login bug"
        ));

        // PreToolUse with toolCall
        let pre_tool = r#"{"source":"antigravity","hook_event":"PreToolUse","payload":{"conversationId":"conv-123","workspacePaths":["/work/my-proj"],"toolCall":{"name":"run_command","args":{"CommandLine":"cargo test"}}}}"#;
        let evs = decode_line(pre_tool, 11);
        assert_eq!(kinds(&evs), vec!["AgentSeen", "ToolStart"]);
        assert!(matches!(
            &evs[1].kind,
            EventKind::ToolStart { id, tool, summary }
            if id == "ag:conv-123" && tool == "run_command" && summary == "cargo test"
        ));

        // PostToolUse: empty string error -> ok: true; non-empty error -> ok: false
        let post_ok = r#"{"source":"antigravity","hook_event":"PostToolUse","payload":{"conversationId":"conv-123","toolCall":{"name":"run_command"},"error":""}}"#;
        let evs = decode_line(post_ok, 12);
        assert!(matches!(&evs[1].kind, EventKind::ToolEnd { tool, ok: true, .. } if tool == "run_command"));

        let post_err = r#"{"source":"antigravity","hook_event":"PostToolUse","payload":{"conversationId":"conv-123","toolCall":{"name":"run_command"},"error":"exit 1"}}"#;
        let evs = decode_line(post_err, 13);
        assert!(matches!(&evs[1].kind, EventKind::ToolEnd { tool, ok: false, .. } if tool == "run_command"));

        // Stop with transcriptPath -> Usage + Boss Inbox Handoff
        let dir = std::env::temp_dir().join(format!("office-ag-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tp = dir.join("transcript.jsonl");
        let t_lines = [
            r###"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-4-neon-high","input_tokens":100,"cache_read_tokens":500,"output_tokens":40,"content":"## All fixed\nPatched auth."}"###,
        ];
        std::fs::write(&tp, t_lines.join("\n")).unwrap();
        let stop = serde_json::json!({
            "source": "antigravity",
            "hook_event": "Stop",
            "payload": {
                "conversationId": "conv-123",
                "workspacePaths": ["/work/my-proj"],
                "modelName": "auto",
                "lastUserInput": "fix the login bug",
                "transcriptPath": tp.to_string_lossy(),
            }
        })
        .to_string();
        let evs = decode_line(&stop, 14);
        assert_eq!(kinds(&evs), vec!["AgentSeen", "State", "Usage", "Handoff"]);
        assert!(matches!(&evs[0].kind, EventKind::AgentSeen { model: None, .. }));
        assert!(matches!(&evs[1].kind, EventKind::State { state: AgentState::Idle, .. }));
        assert!(matches!(
            &evs[2].kind,
            EventKind::Usage { id, tokens_in: 100, tokens_out: 40, tokens_cache_read: 500, model: Some(m), .. }
            if id == "ag:conv-123" && m == "gemini-4-neon-high"
        ));
        assert!(matches!(
            &evs[3].kind,
            EventKind::Handoff { from, to: None, kind: ReportKind::Result, title, body, brief }
            if from == "ag:conv-123" && title == "All fixed" && body == "## All fixed\nPatched auth." && brief == "fix the login bug"
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Integration: listener on a temp socket <- built office-hook binary <- sample PreToolUse JSON.
    #[test]
    fn office_hook_binary_delivers_event() {
        let bin = office_hook_bin();
        let dir = std::env::temp_dir().join(format!("office-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");
        let listener = bind(&sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve(listener, tx));

        // Warm-up exec: the first launch of a freshly built binary on macOS pays a one-off scan cost.
        let _ = Command::new(&bin).env("OFFICE_SOCKET", dir.join("none.sock")).stdin(Stdio::null()).output();

        let mut child = Command::new(&bin)
            .env("OFFICE_SOCKET", &sock)
            .env("OFFICE_SOURCE", "claude")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let sample = r#"{"session_id":"itest","cwd":"/tmp/proj","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/tmp/proj/lib.rs"},"tool_use_id":"toolu_1"}"#;
        child.stdin.take().unwrap().write_all(sample.as_bytes()).unwrap();
        let t0 = std::time::Instant::now();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "office-hook must be silent");
        assert!(t0.elapsed() < Duration::from_millis(300), "office-hook too slow: {:?}", t0.elapsed());

        let mut got = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(2)) {
            let done = matches!(e.kind, EventKind::ToolStart { .. });
            got.push(e);
            if done {
                break;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            got.iter().any(|e| matches!(&e.kind, EventKind::ToolStart { id, tool, summary } if id == "cc:itest" && tool == "Read" && summary == "lib.rs")),
            "no ToolStart received: {got:?}"
        );
    }

    #[test]
    fn office_hook_exits_zero_without_listener() {
        let bin = office_hook_bin();
        let mut child = Command::new(&bin)
            .env("OFFICE_SOCKET", "/nonexistent/dir/x.sock")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"{}").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stdout.is_empty());
    }

    #[test]
    fn office_hook_antigravity_exits_silently() {
        let bin = office_hook_bin();
        let mut child = Command::new(&bin)
            .env("OFFICE_SOCKET", "/nonexistent/dir/x.sock")
            .env("OFFICE_SOURCE", "antigravity")
            .args(["--hook-event", "Stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"{}").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn tool_summary_formats_grep_agent_bash_and_mcp() {
        assert_eq!(
            tool_summary("Grep", &serde_json::json!({"pattern": "foo", "path": "/a/b/src"})),
            "foo @ src"
        );
        assert_eq!(
            tool_summary("Agent", &serde_json::json!({"subagent_type": "Explore", "description": "find routes"})),
            "Explore: find routes"
        );
        assert_eq!(
            tool_summary("Bash", &serde_json::json!({"description": "run tests", "command": "cargo test"})),
            "run tests"
        );
        assert_eq!(
            tool_summary("Bash", &serde_json::json!({"command": "cargo test"})),
            "cargo test"
        );
        assert_eq!(tool_summary("mcp__github__list_issues", &serde_json::json!({})), "github:list_issues");
    }
}
