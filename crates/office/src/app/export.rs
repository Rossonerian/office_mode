//! Clipboard and file export for the report reader (`y` copy, `w` save). The real work lives here;
//! tests go through the `Control` hooks instead of touching pbcopy or the disk.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Copy text to the system clipboard without blocking the UI: the data is written and the child reaped on a thread.
pub fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    #[cfg(target_os = "windows")]
    let mut child = Command::new("clip").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut child = {
        let candidates: &[(&str, &[&str])] = &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        let mut spawned = None;
        for (bin, args) in candidates {
            if let Ok(c) = Command::new(bin).args(*args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                spawned = Some(c);
                break;
            }
        }
        spawned.ok_or_else(|| anyhow::anyhow!("no clipboard tool found (install wl-copy, xclip, or xsel)"))?
    };
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("clipboard command has no stdin"))?;
    let data = text.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(data.as_bytes());
        drop(stdin);
        let _ = child.wait();
    });
    Ok(())
}

/// `~/Downloads` (or the platform's download dir).
pub fn downloads_dir() -> Option<PathBuf> {
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
}

/// Write `content` to `<dir>/<name>`, creating the directory.
pub fn save_in(dir: &Path, name: &str, content: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Save into the downloads folder; returns the path with `~` for the home directory.
pub fn save_to_downloads(name: &str, content: &str) -> anyhow::Result<String> {
    let dir = downloads_dir().ok_or_else(|| anyhow::anyhow!("no Downloads folder"))?;
    let path = save_in(&dir, name, content)?;
    Ok(tilde(&path))
}

pub fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(&h).ok().map(|p| p.to_path_buf())) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
