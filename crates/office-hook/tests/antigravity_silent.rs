use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn antigravity_hooks_exit_silently_across_all_events() {
    let bin = env!("CARGO_BIN_EXE_office-hook");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_dir = std::env::temp_dir().join(format!(
        "office-hook-agy-test-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create isolated temp dir");
    let socket_path = temp_dir.join("office.sock");

    let events = [
        ("SessionStart", r#"{"session_id":"test-session","cwd":"/tmp"}"#),
        ("PreInvocation", r#"{"session_id":"test-session","invocation_id":"inv-1"}"#),
        ("PostInvocation", r#"{"session_id":"test-session","invocation_id":"inv-1"}"#),
        (
            "PreToolUse",
            r#"{"session_id":"test-session","tool_name":"run_command","tool_use_id":"tool-1","tool_input":{"command":"echo hi"}}"#,
        ),
        (
            "PostToolUse",
            r#"{"session_id":"test-session","tool_name":"run_command","tool_use_id":"tool-1"}"#,
        ),
        ("Stop", r#"{"session_id":"test-session"}"#),
    ];

    for (event, payload) in events {
        let mut child = Command::new(bin)
            .args(["--hook-event", event])
            .env("OFFICE_SOURCE", "antigravity")
            .env("OFFICE_RUNTIME_DIR", &temp_dir)
            .env("OFFICE_SOCKET", &socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn office-hook for {event}: {e}"));

        {
            let stdin = child.stdin.as_mut().expect("open stdin");
            stdin
                .write_all(payload.as_bytes())
                .unwrap_or_else(|e| panic!("write stdin for {event}: {e}"));
        }

        let output = child
            .wait_with_output()
            .unwrap_or_else(|e| panic!("wait office-hook for {event}: {e}"));

        assert!(
            output.status.success(),
            "{event}: expected exit 0, got {:?}",
            output.status
        );
        assert!(
            output.stdout.is_empty(),
            "{event}: expected empty stdout, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stderr.is_empty(),
            "{event}: expected empty stderr, got {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}
