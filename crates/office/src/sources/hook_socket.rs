//! Unix socket listener for office-hook lines; decodes Claude Code and Antigravity hook payloads into Events.
use super::{basename, short};
use office_core::model::{AgentKind, AgentState};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Per-line cap (office-hook caps stdin at 1 MB; leave room for the envelope).
const MAX_LINE: u64 = (1 << 20) + 64 * 1024;

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let listener = bind(&paths::hook_socket())?;
    std::thread::Builder::new().name("hook-socket".into()).spawn(move || serve(listener, tx))?;
    Ok(())
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
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            anyhow::bail!("another office is already listening on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(l)
}

pub fn serve(listener: UnixListener, tx: Sender<Event>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let tx = tx.clone();
        let _ = std::thread::Builder::new().name("hook-conn".into()).spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut r = BufReader::new(stream.take(MAX_LINE * 4));
            let mut line = String::new();
            loop {
                line.clear();
                match (&mut r).take(MAX_LINE).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        for ev in decode_line(line.trim(), paths::now_ms()) {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
    }
}

fn s<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty())
}

/// Short human summary of a tool call: file basename, first 40 chars of a command, pattern, url…
pub fn tool_summary(tool: &str, input: &Value) -> String {
    if let Some(p) = s(input, &["file_path", "notebook_path", "path"]) {
        return short(&basename(p), 40);
    }
    if let Some(c) = s(input, &["command"]) {
        return short(c, 40);
    }
    if matches!(tool, "Agent" | "Task") {
        if let Some(d) = s(input, &["subagent_type", "description"]) {
            return short(d, 40);
        }
    }
    if tool == "AskUserQuestion" {
        if let Some(q) = input.get("questions").and_then(|q| q.get(0)).and_then(|q| q.get("question")).and_then(Value::as_str) {
            return short(q, 40);
        }
    }
    if let Some(x) = s(input, &["pattern", "url", "query", "description", "prompt"]) {
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
                    label: Some(agent_type.clone().unwrap_or_else(|| "sub".into())),
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
        "SessionStart" | "SubagentStart" => {}
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
        "Stop" => out.push(ev(at, src, state(AgentState::Idle, Some(String::new())))),
        "StopFailure" => out.push(ev(at, src, state(AgentState::Error, s(p, &["error", "message"]).map(|x| short(x, 40))))),
        "SubagentStop" => {
            out.push(ev(at, src, state(AgentState::Done, None)));
            if agent_id.is_some() {
                out.push(ev(at, src, EventKind::Gone { id: id.clone() }));
            }
        }
        "SessionEnd" => out.push(ev(at, src, EventKind::Gone { id: id.clone() })),
        _ => {}
    }
    out
}

/// Antigravity CLI hook payloads: field names vary, so accept snake/camel variants.
fn decode_antigravity(p: &Value, hook_event: Option<&str>, _request_id: Option<String>, at: u64) -> Vec<Event> {
    let name = hook_event.or_else(|| s(p, &["hook_event_name", "hookEventName", "event"])).unwrap_or("");
    let cwd = s(p, &["cwd", "workspace", "workspaceRoot"]).map(str::to_string);
    let sid = s(p, &["session_id", "sessionId", "conversation_id", "conversationId"])
        .map(str::to_string)
        .or_else(|| cwd.clone().map(|c| format!("cwd-{}", paths::sanitize_id(&basename(&c)))));
    let Some(sid) = sid else { return vec![] };
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
            cwd: cwd.clone(),
            role: None,
            model: s(p, &["model"]).map(str::to_string),
            slot: None,
            pid: p.get("pid").and_then(Value::as_u64).map(|x| x as u32),
            resume_id: None,
        },
    )];
    let tool = s(p, &["tool_name", "toolName", "tool"]).unwrap_or("").to_string();
    let input = p.get("tool_input").or_else(|| p.get("toolInput")).or_else(|| p.get("args")).cloned().unwrap_or(Value::Null);
    match name {
        "PreToolUse" | "BeforeTool" => {
            out.push(ev(at, src, EventKind::ToolStart { id: id.clone(), tool: tool.clone(), summary: tool_summary(&tool, &input) }))
        }
        "PostToolUse" | "AfterTool" => {
            let ok = p.get("error").map(Value::is_null).unwrap_or(true);
            out.push(ev(at, src, EventKind::ToolEnd { id: id.clone(), tool, ok }))
        }
        "UserPromptSubmit" | "BeforeAgent" => {
            out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Thinking, activity: None }))
        }
        "Stop" | "AfterAgent" => {
            out.push(ev(at, src, EventKind::State { id: id.clone(), state: AgentState::Idle, activity: Some(String::new()) }))
        }
        "SessionEnd" => out.push(ev(at, src, EventKind::Gone { id })),
        _ => {}
    }
    out
}

/// Build office-hook (no-op when fresh) and return its path, from cargo's JSON messages.
#[cfg(test)]
fn office_hook_bin() -> std::path::PathBuf {
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
        .find_map(|v| v["executable"].as_str().map(std::path::PathBuf::from))
        .expect("office-hook executable path")
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
        assert_eq!(kinds(&decode_line(stop, 1)), vec!["AgentSeen", "State", "Gone"]);
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
}
