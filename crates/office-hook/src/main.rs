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
const STDIN_TIMEOUT: Duration = Duration::from_millis(200);
const SEND_TIMEOUT: Duration = Duration::from_millis(150);

fn main() {
    // Never let a panic produce a non-zero exit or noise.
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(run);
    std::process::exit(0);
}

fn read_stdin() -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(STDIN_TIMEOUT).ok()
}

/// Send one line to the socket with an overall deadline (UnixStream has no connect timeout).
fn send_line(line: String) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let ok = (|| -> std::io::Result<()> {
            let mut s = UnixStream::connect(paths::hook_socket())?;
            s.set_write_timeout(Some(SEND_TIMEOUT))?;
            s.write_all(line.as_bytes())?;
            s.write_all(b"\n")?;
            s.flush()
        })()
        .is_ok();
        let _ = tx.send(ok);
    });
    let _ = rx.recv_timeout(SEND_TIMEOUT);
}

fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let permission = args.iter().any(|a| a == "--permission");
    let hook_event = args.iter().position(|a| a == "--hook-event").and_then(|i| args.get(i + 1)).cloned();

    let raw = read_stdin().unwrap_or_default();
    let payload: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) if raw.is_empty() => Value::Null,
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
    send_line(line.to_string());

    let Some(request_id) = request_id else { return };
    let timeout_s = office_core::config::Config::load().permission_timeout_s;
    if timeout_s == 0 {
        return;
    }
    let decision = wait_decision(&request_id, Duration::from_secs(timeout_s));
    // Tell the office the request is no longer pending (decided or timed out).
    let mut done = json!({
        "hook_event_name": "OfficePermissionResolved",
        "request_id": request_id,
        "session_id": payload.get("session_id").cloned().unwrap_or(Value::Null),
    });
    if let Some(a) = payload.get("agent_id") {
        done["agent_id"] = a.clone();
    }
    send_line(json!({ "source": source, "payload": done, "ts": paths::now_ms(), "request_id": request_id }).to_string());

    if let Some(allow) = decision {
        let out = decision_json(allow);
        let mut so = std::io::stdout();
        let _ = writeln!(so, "{out}");
        let _ = so.flush();
    }
}

/// Poll <runtime_dir>/decisions/<id> for "allow"/"deny". None on timeout.
fn wait_decision(request_id: &str, timeout: Duration) -> Option<bool> {
    let path = paths::decisions_dir().join(request_id);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_ascii_lowercase();
            if s == "allow" || s == "deny" {
                let _ = std::fs::remove_file(&path);
                return Some(s == "allow");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
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
