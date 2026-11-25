use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runtime dir for the hook socket: $TMPDIR/agent-office-<uid>/ (0700).
pub fn runtime_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OFFICE_RUNTIME_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let uid = std::env::var("UID").ok().or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "u".into());
    std::env::temp_dir().join(format!("agent-office-{uid}"))
}

/// Unix socket the TUI listens on; office-hook writes one JSON line per hook event.
/// `$OFFICE_SOCKET` overrides the path (tests, multiple offices).
pub fn hook_socket() -> PathBuf {
    if let Ok(p) = std::env::var("OFFICE_SOCKET") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    runtime_dir().join("office.sock")
}

/// Where the TUI drops "allow"/"deny" for a waiting `office-hook --permission`.
pub fn decisions_dir() -> PathBuf {
    runtime_dir().join("decisions")
}

/// Request id for a Claude PermissionRequest payload: its `tool_use_id`, else a stable FNV-1a hash
/// of the payload's JSON text. office-hook also sends it in the socket line (`request_id`).
pub fn permission_request_id(payload: &serde_json::Value) -> String {
    if let Some(id) = payload.get("tool_use_id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            return sanitize_id(id);
        }
    }
    let text = serde_json::to_string(payload).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("req-{h:016x}")
}

/// Keep ids safe as file names.
pub fn sanitize_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(128).collect()
}

/// Config file: ~/.config/agent-office/config.toml
pub fn config_file() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/agent-office/config.toml")
}

/// Theme search path: ~/.config/agent-office/themes, then <repo>/assets/themes (via $OFFICE_ASSETS).
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut v = vec![dirs::home_dir().unwrap_or_default().join(".config/agent-office/themes")];
    if let Ok(p) = std::env::var("OFFICE_ASSETS") {
        v.push(PathBuf::from(p).join("themes"));
    }
    v
}

pub fn claude_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude")
}

pub fn agy_run_dir() -> PathBuf {
    run_dir()
}

/// `$OFFICE_RUN_DIR`, else `~/.claude/run`.
pub fn run_dir() -> PathBuf {
    match std::env::var_os("OFFICE_RUN_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => claude_dir().join("run"),
    }
}
