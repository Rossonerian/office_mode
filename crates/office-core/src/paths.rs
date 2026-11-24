use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runtime dir for the hook socket: $TMPDIR/agent-office-<uid>/ (0700).
pub fn runtime_dir() -> PathBuf {
    let uid = std::env::var("UID").ok().or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "u".into());
    std::env::temp_dir().join(format!("agent-office-{uid}"))
}

/// Unix socket the TUI listens on; office-hook writes one JSON line per hook event.
pub fn hook_socket() -> PathBuf {
    runtime_dir().join("office.sock")
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
    claude_dir().join("run")
}
