//! Workstream A. Control actions invoked by the TUI (workstream C).
use office_core::paths;
use std::io::Write;
use std::process::{Command, Stdio};

/// SIGTERM an agy-route router process (it releases its slot and kills its agy group).
pub fn cancel_agy(router_pid: u32) -> anyhow::Result<()> {
    anyhow::ensure!(router_pid > 1, "refusing to signal pid {router_pid}");
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(router_pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return Err(anyhow::anyhow!("kill -TERM {router_pid} failed: {}", std::io::Error::last_os_error()));
        }
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let st = Command::new("kill").args(["-TERM", &router_pid.to_string()]).stderr(Stdio::null()).status()?;
        anyhow::ensure!(st.success(), "kill -TERM {router_pid} failed");
        Ok(())
    }
}

/// Answer a pending Claude permission request that office-hook --permission is waiting on
/// (write "allow"/"deny" to <runtime_dir>/decisions/<request_id>).
pub fn resolve_permission(request_id: &str, allow: bool) -> anyhow::Result<()> {
    let id = paths::sanitize_id(request_id);
    anyhow::ensure!(!id.is_empty(), "empty request id");
    let dir = paths::decisions_dir();
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, if allow { "allow" } else { "deny" })?;
    std::fs::rename(&tmp, dir.join(&id))?;
    Ok(())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The resume command line for a session.
pub fn resume_command(cwd: Option<&str>, session_id: &str) -> String {
    let resume = format!("claude --resume {}", sh_quote(session_id));
    match cwd {
        Some(c) if !c.is_empty() => format!("cd {} && {resume}", sh_quote(c)),
        _ => resume,
    }
}

/// Resume command for a Claude session: copy `cd <cwd> && claude --resume <id>` to the clipboard (pbcopy)
/// asynchronously and try to open a new Ghostty window running it inside the user's login shell.
pub fn resume_claude(cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
    let cmd = resume_command(cwd, session_id);
    let clip = cmd.clone();
    std::thread::spawn(move || {
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(clip.as_bytes());
            }
            let _ = child.wait();
        }
    });
    let ghostty = std::path::Path::new("/Applications/Ghostty.app").exists()
        || dirs::home_dir().map(|h| h.join("Applications/Ghostty.app").exists()).unwrap_or(false);
    if ghostty {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        // Run inside the user's interactive login shell so `claude` is on PATH, and keep it open on exit.
        let inner = format!("{cmd}; exec {} -l", sh_quote(&shell));
        if let Ok(child) = Command::new("open")
            .args(["-na", "Ghostty", "--args", "-e", &shell, "-lic", &inner])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            reap(child);
        }
    }
    Ok(cmd)
}

/// Non-blocking macOS chime when `sound = true` (no-op elsewhere).
pub fn play_sound(critical: bool) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let path = if critical {
        "/System/Library/Sounds/Sosumi.aiff"
    } else {
        "/System/Library/Sounds/Glass.aiff"
    };
    if !std::path::Path::new(path).exists() {
        return;
    }
    if let Ok(child) = Command::new("afplay").arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        reap(child);
    }
}

fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn as_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " ")
}

/// macOS notification via osascript (no-op elsewhere). Must not block (> 200 ms).
pub fn notify(title: &str, body: &str) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let script = format!("display notification \"{}\" with title \"{}\"", as_escape(body), as_escape(title));
    if let Ok(child) =
        Command::new("osascript").args(["-e", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    {
        reap(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_command_quotes() {
        assert_eq!(resume_command(Some("/a b/it's"), "s1"), r"cd '/a b/it'\''s' && claude --resume 's1'");
        assert_eq!(resume_command(None, "s1"), "claude --resume 's1'");
    }
    #[test]
    fn escapes_applescript() {
        assert_eq!(as_escape("a \"b\" \\ c\nd"), r#"a \"b\" \\ c d"#);
    }
    #[test]
    fn cancel_refuses_init() {
        assert!(cancel_agy(1).is_err());
        assert!(cancel_agy(0).is_err());
    }
}
