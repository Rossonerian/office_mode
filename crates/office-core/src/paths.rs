use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// User theme packs dir: ~/.config/agent-office/themes
pub fn user_themes_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/agent-office/themes")
}

/// User CEO outfit packs dir: ~/.config/agent-office/outfits
pub fn outfits_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/agent-office/outfits")
}

/// User custom sound slots dir: ~/.config/agent-office/sounds
pub fn sounds_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/agent-office/sounds")
}

/// Persistent whiteboard file: ~/.config/agent-office/whiteboard.json
pub fn whiteboard_file() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/agent-office/whiteboard.json")
}

/// Ensure `runtime_dir()` exists with 0700 permissions on Unix, returning its path.
pub fn ensure_runtime_dir() -> std::io::Result<PathBuf> {
    let dir = runtime_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

/// Atomically write `bytes` to `path` via a unique same-directory temp file (`create_new(true)`).
/// Refuses to write if `path` already exists as a symlink.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("refusing to write through symlink: {}", path.display()),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!("{fname}.{}.{seq}.tmp", std::process::id()));

    let write_res = (|| -> std::io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = replace_file(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    Ok(())
}

#[cfg(unix)]
fn replace_file(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dst)
}

#[cfg(windows)]
fn replace_file(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let src_w: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
    let dst_w: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    const ERROR_SHARING_VIOLATION: i32 = 32;

    for attempt in 0..4 {
        let ok = unsafe {
            MoveFileExW(
                src_w.as_ptr(),
                dst_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) && attempt < 3 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            continue;
        }
        return Err(err);
    }
    Err(std::io::Error::last_os_error())
}

#[cfg(not(any(unix, windows)))]
fn replace_file(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dst)
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

    fn test_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "office-paths-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn atomic_write_creates_and_overwrites_cleanly() {
        let dir = test_dir("atomic");
        let target = dir.join("nested/state.json");

        atomic_write(&target, b"first payload").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"first payload");

        atomic_write(&target, b"second payload").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second payload");

        // Ensure no leftover .tmp files in parent dir.
        let leftover: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink_destination() {
        let dir = test_dir("symlink");
        let real = dir.join("real.txt");
        let link = dir.join("link.txt");
        std::fs::write(&real, b"untouched").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = atomic_write(&link, b"evil").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&real).unwrap(), b"untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
