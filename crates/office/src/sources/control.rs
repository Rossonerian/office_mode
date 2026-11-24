//! Workstream A. Control actions invoked by the TUI (workstream C).

/// SIGTERM an agy-route router process (it releases its slot and kills its agy group).
pub fn cancel_agy(_router_pid: u32) -> anyhow::Result<()> {
    todo!("workstream A")
}

/// Answer a pending Claude permission request that office-hook --permission is waiting on
/// (write "allow"/"deny" to <runtime_dir>/decisions/<request_id>).
pub fn resolve_permission(_request_id: &str, _allow: bool) -> anyhow::Result<()> {
    todo!("workstream A")
}

/// Resume command for a Claude session: copy `cd <cwd> && claude --resume <id>` to the clipboard (pbcopy)
/// and try to open a new Ghostty window running it. Returns the command string.
pub fn resume_claude(_cwd: Option<&str>, _session_id: &str) -> anyhow::Result<String> {
    todo!("workstream A")
}

/// macOS notification via osascript (no-op elsewhere). Must not block (> 200 ms).
pub fn notify(_title: &str, _body: &str) {
    todo!("workstream A")
}
