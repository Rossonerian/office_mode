use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runtime dir for the hook socket / IPC descriptor: $TMPDIR/agent-office-<uid>/ (0700 on Unix).
pub fn runtime_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OFFICE_RUNTIME_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let uid = std::env::var("UID")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .map(|s| sanitize_id(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "u".into());
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

fn push_unique(out: &mut Vec<PathBuf>, p: PathBuf) {
    if !p.as_os_str().is_empty() && !out.contains(&p) {
        out.push(p);
    }
}

/// Theme search path:
/// 1. `~/.config/agent-office/themes`
/// 2. `$OFFICE_ASSETS/themes`
/// 3. Bundled themes next to the executable or in `../share/office_mode/assets/themes`
/// 4. Standard system share paths on Unix
pub fn theme_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let office_assets = std::env::var("OFFICE_ASSETS").ok();
    let mut exes = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        exes.push(exe.clone());
        if let Ok(canon) = std::fs::canonicalize(&exe) {
            if canon != exe {
                exes.push(canon);
            }
        }
    }
    theme_dirs_from(home.as_deref(), office_assets.as_deref(), &exes)
}

fn theme_dirs_from(home: Option<&Path>, office_assets: Option<&str>, exes: &[PathBuf]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = home {
        push_unique(&mut v, h.join(".config/agent-office/themes"));
    }
    if let Some(p) = office_assets.filter(|s| !s.is_empty()) {
        push_unique(&mut v, PathBuf::from(p).join("themes"));
    }
    for exe in exes {
        if let Some(bin_dir) = exe.parent() {
            push_unique(&mut v, bin_dir.join("assets/themes"));
            if let Some(prefix) = bin_dir.parent() {
                push_unique(&mut v, prefix.join("share/office_mode/assets/themes"));
                push_unique(&mut v, prefix.join("share/agent-office/assets/themes"));
            }
        }
    }
    #[cfg(unix)]
    {
        push_unique(&mut v, PathBuf::from("/opt/homebrew/share/office_mode/assets/themes"));
        push_unique(&mut v, PathBuf::from("/usr/local/share/office_mode/assets/themes"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_dirs_prioritize_user_env_and_exe_locations() {
        let home = PathBuf::from("/home/u");
        let exes = vec![PathBuf::from("/usr/local/bin/office"), PathBuf::from("/opt/pkg/bin/office")];
        let dirs = theme_dirs_from(Some(&home), Some("/custom/assets"), &exes);
        assert_eq!(dirs[0], PathBuf::from("/home/u/.config/agent-office/themes"));
        assert_eq!(dirs[1], PathBuf::from("/custom/assets/themes"));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin/assets/themes")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/share/office_mode/assets/themes")));
        assert!(dirs.contains(&PathBuf::from("/opt/pkg/share/office_mode/assets/themes")));
        // Deduplicated
        let unique: std::collections::HashSet<&PathBuf> = dirs.iter().collect();
        assert_eq!(unique.len(), dirs.len());
    }
}
