//! office-hook: called by agent CLIs per hook event. Owned by workstream A. MUST always exit 0 quickly.
//!
//! Reads the hook JSON on stdin (<= 1 MB, 200 ms), sends ONE line
//! `{"source":..,"payload":<raw>,"ts":..[,"hook_event":..][,"request_id":..]}` to the office socket
//! (150 ms budget) and exits 0 silently.
//! Flags: `--hook-event <Name>` (Antigravity hooks don't always name the event in the payload),
//! `--permission` (PermissionRequest: wait up to config.permission_timeout_s for a decision file).
use office_core::paths;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAX_STDIN: u64 = 1 << 20;
const TOTAL_BUDGET: Duration = Duration::from_millis(280);
const STDIN_TIMEOUT: Duration = Duration::from_millis(150);
const SEND_CAP: Duration = Duration::from_millis(120);
const STALE_DECISION_AGE: Duration = Duration::from_secs(3600);

fn main() {
    // Never let a panic produce a non-zero exit or noise.
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(run);
    std::process::exit(0);
}

fn read_stdin(timeout: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(timeout).ok()
}

/// Send one line to the socket with an overall deadline (UnixStream has no connect timeout).
/// Returns true only if the office received it (i.e. the office is running).
fn send_line(line: String, timeout: Duration) -> bool {
    if timeout.is_zero() {
        return false;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let ok = (|| -> std::io::Result<()> {
            let mut s = UnixStream::connect(paths::hook_socket())?;
            s.set_write_timeout(Some(timeout))?;
            s.write_all(line.as_bytes())?;
            s.write_all(b"\n")?;
            s.flush()
        })()
        .is_ok();
        let _ = tx.send(ok);
    });
    rx.recv_timeout(timeout).unwrap_or(false)
}

fn run() {
    let start = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let permission = args.iter().any(|a| a == "--permission");
    let hook_event = args.iter().position(|a| a == "--hook-event").and_then(|i| args.get(i + 1)).cloned();

    let raw = read_stdin(STDIN_TIMEOUT).unwrap_or_default();
    let payload: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) if raw.is_empty() || raw.len() as u64 >= MAX_STDIN => Value::Null,
        Err(_) => Value::String(String::from_utf8_lossy(&raw).into_owned()),
    };
    let source = std::env::var("OFFICE_SOURCE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "claude".into());

    let mut line = json!({ "source": source, "payload": payload, "ts": paths::now_ms() });
    if let Some(e) = &hook_event {
        line["hook_event"] = json!(e);
    }
    let request_id = permission.then(|| paths::permission_request_id(&payload));
    if let Some(id) = &request_id {
        line["request_id"] = json!(id);
    }
    let remaining = TOTAL_BUDGET.saturating_sub(start.elapsed()).min(SEND_CAP);
    let delivered = send_line(line.to_string(), remaining);

    // Non-permission observer hooks must exit 0 silently: printing `{}` on Antigravity
    // PreToolUse fails closed (missing `decision`), while `{"decision":"allow"}` would bypass prompts.
    let Some(request_id) = request_id else { return };
    // Office not running: never make the user wait; fall through to the normal Claude prompt.
    if !delivered {
        return;
    }
    let timeout_s = office_core::config::Config::load().permission_timeout_s;
    let decision = if timeout_s > 0 {
        wait_decision(&request_id, Duration::from_secs(timeout_s))
    } else {
        None
    };
    // Tell the office the request is no longer pending (decided, timed out, or permission_timeout_s == 0).
    let mut done = json!({
        "hook_event_name": "OfficePermissionResolved",
        "request_id": request_id,
        "session_id": payload.get("session_id").cloned().unwrap_or(Value::Null),
    });
    if let Some(a) = payload.get("agent_id") {
        done["agent_id"] = a.clone();
    }
    send_line(json!({ "source": source, "payload": done, "ts": paths::now_ms(), "request_id": request_id }).to_string(), SEND_CAP);

    if let Some(allow) = decision {
        let out = decision_json(allow);
        let mut so = std::io::stdout();
        let _ = writeln!(so, "{out}");
        let _ = so.flush();
    }
}

/// Poll <runtime_dir>/decisions/<id> for "allow"/"deny". None on timeout.
fn wait_decision(request_id: &str, timeout: Duration) -> Option<bool> {
    let dir = paths::decisions_dir();
    let path = dir.join(request_id);
    // Drop any stale decision file left by an earlier aborted run with the same id.
    let _ = std::fs::remove_file(&path);
    let deadline = Instant::now() + timeout;
    let mut res = None;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_ascii_lowercase();
            if s == "allow" || s == "deny" {
                let _ = std::fs::remove_file(&path);
                res = Some(s == "allow");
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    prune_old_decisions(&dir);
    res
}

fn prune_old_decisions(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if let Ok(age) = e.metadata().and_then(|m| m.modified()).and_then(|m| m.elapsed().map_err(std::io::Error::other)) {
            if age > STALE_DECISION_AGE {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Claude Code PermissionRequest hook output (hookSpecificOutput.decision.behavior).
fn decision_json(allow: bool) -> Value {
    let decision = if allow {
        json!({ "behavior": "allow" })
    } else {
        json!({ "behavior": "deny", "message": "Denied from agent-office" })
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decision_shapes() {
        let a = decision_json(true);
        assert_eq!(a["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(a["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(decision_json(false)["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }
}
