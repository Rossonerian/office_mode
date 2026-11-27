//! Workstream A. Control actions invoked by the TUI (workstream C).
use office_core::{model::AgentKind, paths};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Terminate an agy-route router process (`SIGTERM` on Unix, `taskkill` on Windows).
pub fn cancel_agy(router_pid: u32) -> anyhow::Result<()> {
    anyhow::ensure!(router_pid > 1, "refusing to signal pid {router_pid}");
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(router_pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return Err(anyhow::anyhow!("kill -TERM {router_pid} failed: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let st = Command::new("taskkill")
            .args(["/PID", &router_pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        anyhow::ensure!(st.success(), "taskkill /PID {router_pid} failed");
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let st = Command::new("kill").args(["-TERM", &router_pid.to_string()]).stderr(Stdio::null()).status()?;
        anyhow::ensure!(st.success(), "kill -TERM {router_pid} failed");
        Ok(())
    }
}

fn exe_stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// True when `comm`/`cmd` identify a killable agent CLI or `agy-route` process (never `office`,
/// shells, `launchd`, or Electron/Antigravity GUI helpers).
pub fn is_killable_agent_cmd(comm: &str, cmd: &str) -> bool {
    if comm.contains(".app/Contents/")
        || cmd.contains(".app/Contents/")
        || comm.contains("Antigravity Helper")
        || cmd.contains("Antigravity Helper")
    {
        return false;
    }
    let comm_base = exe_stem(&super::basename(comm.trim()));
    let cmd_base = exe_stem(&super::basename(cmd.split_whitespace().next().unwrap_or("")));
    let is_base = |name: &str| comm_base == name || cmd_base == name;
    if is_base("claude") || is_base("agy") {
        return true;
    }
    if is_base("bun") && (cmd.contains("agy-route") || cmd.contains("rosso-auto") || cmd.contains("rosso-run")) {
        return true;
    }
    if is_base("node")
        && (cmd.contains("/claude") || cmd.contains("\\claude") || cmd.contains("claude-code") || cmd.contains("/agy") || cmd.contains("\\agy"))
    {
        return true;
    }
    false
}

#[cfg(unix)]
fn ps_comm_cmd(pid: u32) -> Option<(String, String)> {
    let pid_s = pid.to_string();
    let comm_out = Command::new("/bin/ps")
        .args(["-o", "comm=", "-p", &pid_s])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !comm_out.status.success() {
        return None;
    }
    let cmd_out = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid_s])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !cmd_out.status.success() {
        return None;
    }
    let comm = String::from_utf8_lossy(&comm_out.stdout).trim().to_string();
    let cmd = String::from_utf8_lossy(&cmd_out.stdout).trim().to_string();
    if comm.is_empty() && cmd.is_empty() {
        return None;
    }
    Some((comm, cmd))
}

#[cfg(windows)]
fn ps_comm_cmd(pid: u32) -> Option<(String, String)> {
    let filter = format!("PID eq {pid}");
    let out = Command::new("tasklist")
        .args(["/FI", &filter, "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    if !line.starts_with('"') {
        return None;
    }
    let image = line.split(',').next()?.trim_matches('"').to_string();
    if image.is_empty() {
        return None;
    }
    Some((image.clone(), image))
}

fn is_refused_pid(pid: u32) -> bool {
    if pid <= 1 || pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    if pid == unsafe { libc::getppid() } as u32 {
        return true;
    }
    false
}

/// Safely terminate an agent process by PID: refuses init/self/parent, verifies process identity,
/// sends `SIGTERM` -> `SIGKILL` on Unix or `taskkill /T /F` on Windows.
pub fn kill_agent_pid(pid: u32) -> anyhow::Result<()> {
    anyhow::ensure!(!is_refused_pid(pid), "refusing to signal pid {pid}");
    let (comm, cmd) = ps_comm_cmd(pid).ok_or_else(|| anyhow::anyhow!("process {pid} not found"))?;
    anyhow::ensure!(
        is_killable_agent_cmd(&comm, &cmd),
        "refusing to signal non-agent process {pid} ({comm})"
    );
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            return Err(anyhow::anyhow!("kill -TERM {pid} failed: {}", std::io::Error::last_os_error()));
        }
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(2500);
            while Instant::now() < deadline {
                if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            if !is_refused_pid(pid) && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                if let Some((comm, cmd)) = ps_comm_cmd(pid) {
                    if is_killable_agent_cmd(&comm, &cmd) {
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                    }
                }
            }
        });
        Ok(())
    }
    #[cfg(windows)]
    {
        let st = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        anyhow::ensure!(st.success(), "taskkill /PID {pid} failed");
        Ok(())
    }
}

/// Find and terminate orphan `agy` CLI processes (`ppid == 1`, outside any live router tree).
/// Returns `(killed_count, freed_mb)`.
#[cfg(unix)]
pub fn reap_orphan_agy() -> anyhow::Result<(usize, u32)> {
    let out = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,rss=,command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    anyhow::ensure!(out.status.success(), "ps failed");
    let procs = super::agy_router::parse_ps(&String::from_utf8_lossy(&out.stdout));
    let routers = super::agy_router::find_routers(&procs);
    let orphans = super::agy_router::find_orphan_agy(&procs, &routers);
    let mut killed = 0usize;
    let mut freed_mb = 0u32;
    for (pid, rss_mb) in orphans {
        if kill_agent_pid(pid).is_ok() {
            killed += 1;
            freed_mb = freed_mb.saturating_add(rss_mb);
        }
    }
    Ok((killed, freed_mb))
}

#[cfg(not(unix))]
pub fn reap_orphan_agy() -> anyhow::Result<(usize, u32)> {
    Ok((0, 0))
}

/// Answer a pending Claude permission request that office-hook --permission is waiting on
/// (write "allow"/"deny" to <runtime_dir>/decisions/<request_id>).
pub fn resolve_permission(request_id: &str, allow: bool) -> anyhow::Result<()> {
    let id = paths::sanitize_id(request_id);
    anyhow::ensure!(!id.is_empty(), "empty request id");
    let dir = paths::decisions_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&dir)?;
    }
    let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, if allow { "allow" } else { "deny" })?;
    std::fs::rename(&tmp, dir.join(&id))?;
    Ok(())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn with_cd(cwd: Option<&str>, cmd: &str) -> String {
    match cwd {
        Some(c) if !c.is_empty() => format!("cd {} && {cmd}", sh_quote(c)),
        _ => cmd.to_string(),
    }
}

/// The resume command line for a Claude session.
pub fn resume_command(cwd: Option<&str>, session_id: &str) -> String {
    with_cd(cwd, &format!("claude --resume {}", sh_quote(session_id)))
}

/// The resume command line for an agent by kind (`agy --resume` for Agy/Other, `claude --resume` otherwise).
pub fn resume_command_for(kind: AgentKind, cwd: Option<&str>, session_id: &str) -> String {
    if matches!(kind, AgentKind::Other | AgentKind::Agy) {
        with_cd(cwd, &format!("agy --resume {}", sh_quote(session_id)))
    } else {
        resume_command(cwd, session_id)
    }
}

fn ghostty_exists() -> bool {
    Path::new("/Applications/Ghostty.app").exists()
        || dirs::home_dir().map(|h| h.join("Applications/Ghostty.app").exists()).unwrap_or(false)
}

fn spawn_ghostty(inner_cmd: &str) -> anyhow::Result<bool> {
    if !ghostty_exists() {
        return Ok(false);
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let inner = format!("{inner_cmd}; exec {} -l", sh_quote(&shell));
    let child = Command::new("open")
        .args(["-na", "Ghostty", "--args", "-e", &shell, "-lic", &inner])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    reap(child);
    Ok(true)
}

fn launch_resume(cmd: String, strip_anthropic_env: bool) -> anyhow::Result<String> {
    let _ = crate::app::export::copy_to_clipboard(&cmd);
    let inner = if strip_anthropic_env {
        format!("unset ANTHROPIC_BASE_URL DISABLE_AUTO_COMPACT; {cmd}")
    } else {
        cmd.clone()
    };
    let _ = spawn_ghostty(&inner);
    Ok(cmd)
}

/// Resume command for a Claude session: copy `cd <cwd> && claude --resume <id>` to the clipboard (pbcopy)
/// asynchronously and try to open a new Ghostty window running it inside the user's login shell.
pub fn resume_claude(cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
    launch_resume(resume_command(cwd, session_id), false)
}

/// Resume an agent session by kind: copy the command via pbcopy and open a Ghostty window.
pub fn resume_agent(kind: AgentKind, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
    let strip_env = matches!(kind, AgentKind::Other | AgentKind::Agy);
    launch_resume(resume_command_for(kind, cwd, session_id), strip_env)
}

fn agy_cli_bin() -> &'static str {
    if Path::new("/opt/homebrew/bin/agy").exists() {
        "/opt/homebrew/bin/agy"
    } else {
        "agy"
    }
}

fn spawn_detached_cmd(cmd: &mut Command) -> std::io::Result<()> {
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    reap(child);
    Ok(())
}

fn spawn_claude_detached(args: &[&str], cwd: Option<&Path>) -> anyhow::Result<()> {
    let make = |bin: &Path| {
        let mut c = Command::new(bin);
        c.args(args);
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        c
    };
    match spawn_detached_cmd(&mut make(Path::new("claude"))) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(fallback) = dirs::home_dir().map(|h| h.join(".local/bin/claude")).filter(|p| p.exists()) {
                spawn_detached_cmd(&mut make(&fallback))?;
                Ok(())
            } else {
                Err(e.into())
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn rosso_auto_bin() -> PathBuf {
    if let Some(p) = dirs::home_dir().map(|h| h.join(".claude/bin/rosso-auto")).filter(|p| p.exists()) {
        return p;
    }
    PathBuf::from("rosso-auto")
}

/// Validates `rosso-auto` job names (`^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$`).
fn is_valid_rosso_job(job: &str) -> bool {
    let b = job.as_bytes();
    !b.is_empty()
        && b.len() <= 48
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

static AUTO_JOB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn make_auto_job_name(task: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = true;
    for ch in task.chars() {
        if slug.len() >= 24 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "task" } else { slug };
    let seq = AUTO_JOB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 100;
    let secs = paths::now_ms() / 1000;
    format!("auto-{slug}-{secs}-{seq}")
}

/// Launch a detached `rosso-auto` autonomous pipeline (`flow`: `"council"` or `"simple"`).
pub fn launch_rosso_auto(flow: &str, task: &str, work_dir: Option<&Path>) -> Result<String, String> {
    if !matches!(flow, "council" | "simple") {
        return Err(format!("invalid rosso-auto flow: {flow}"));
    }
    let task = task.trim();
    if task.is_empty() {
        return Err("empty task".into());
    }
    if let Some(d) = work_dir {
        if !d.is_dir() {
            return Err(format!("not a directory: {}", d.display()));
        }
    }
    let job = make_auto_job_name(task);
    let mut cmd = Command::new(rosso_auto_bin());
    cmd.args([job.as_str(), "--task", task, "--flow", flow]);
    if let Some(d) = work_dir {
        let ds = d.to_string_lossy().into_owned();
        cmd.args(["--cwd", &ds]).current_dir(d);
    }
    spawn_detached_cmd(&mut cmd).map_err(|e| format!("failed to launch rosso-auto: {e}"))?;
    Ok(format!("started rosso-auto {job} ({flow})"))
}

/// Resume an existing `rosso-auto` job in the background (`rosso-auto <job> --resume`).
pub fn resume_rosso_auto(job: &str) -> Result<String, String> {
    if !is_valid_rosso_job(job) {
        return Err(format!("invalid rosso-auto job name: {job}"));
    }
    let mut cmd = Command::new(rosso_auto_bin());
    cmd.args([job, "--resume"]);
    spawn_detached_cmd(&mut cmd).map_err(|e| format!("failed to resume rosso-auto {job}: {e}"))?;
    Ok(format!("resumed rosso-auto {job}"))
}

/// Abort a running `rosso-auto` job cleanly (`rosso-auto abort <job>`).
pub fn abort_rosso_auto(job: &str) -> Result<String, String> {
    if !is_valid_rosso_job(job) {
        return Err(format!("invalid rosso-auto job name: {job}"));
    }
    let out = Command::new(rosso_auto_bin())
        .args(["abort", job])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to abort rosso-auto {job}: {e}"))?;
    if out.status.success() {
        Ok(format!("aborted rosso-auto {job}"))
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if msg.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            msg
        };
        if detail.is_empty() {
            Err(format!("rosso-auto abort {job} exited with {}", out.status))
        } else {
            Err(detail)
        }
    }
}

/// Dispatch a detached task to `agy:<role>`, `rosso-auto:<flow>`, `agy-cli`, `claude`, or `claude-win`.
pub fn dispatch_task(target: &str, cwd: &str, prompt: &str, agy_route_bin: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!prompt.trim().is_empty(), "empty prompt");
    let dir: PathBuf = if cwd.trim().is_empty() {
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?
    } else {
        PathBuf::from(cwd)
    };
    anyhow::ensure!(dir.is_dir(), "not a directory: {}", dir.display());
    let dir_str = dir.to_string_lossy().into_owned();

    match target {
        "agy:worker" | "agy:architect" | "agy:supervisor" | "agy:tester" | "agy:manager" | "agy:fallback" | "agy:git" => {
            let role = target.strip_prefix("agy:").expect("matched");
            let bin = if agy_route_bin.trim().is_empty() { "agy-route" } else { agy_route_bin };
            spawn_detached_cmd(Command::new(bin).args(["--role", role, "--cwd", &dir_str, "--prompt", prompt]))?;
        }
        "rosso-auto:council" | "rosso-auto:simple" => {
            let flow = target.strip_prefix("rosso-auto:").expect("matched");
            return launch_rosso_auto(flow, prompt, Some(&dir)).map_err(|e| anyhow::anyhow!(e));
        }
        "agy-cli" => {
            spawn_detached_cmd(
                Command::new(agy_cli_bin())
                    .args(["-p", prompt])
                    .current_dir(&dir)
                    .env_remove("ANTHROPIC_BASE_URL")
                    .env_remove("DISABLE_AUTO_COMPACT"),
            )?;
        }
        "claude" => {
            spawn_claude_detached(&["-p", prompt], Some(&dir))?;
        }
        "claude-win" => {
            let cmd = format!("cd {} && claude {}", sh_quote(&dir_str), sh_quote(prompt));
            anyhow::ensure!(spawn_ghostty(&cmd)?, "Ghostty.app not found");
        }
        _ => anyhow::bail!("unknown dispatch target: {target}"),
    }
    Ok(format!("dispatched {target} in {dir_str}"))
}

/// Send a non-interactive follow-up prompt to an existing session.
pub fn followup_session(kind: AgentKind, cwd: Option<&str>, session_id: &str, prompt: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!session_id.trim().is_empty(), "empty session id");
    anyhow::ensure!(!prompt.trim().is_empty(), "empty prompt");
    let dir = cwd.map(str::trim).filter(|c| !c.is_empty()).map(Path::new);
    if matches!(kind, AgentKind::Other | AgentKind::Agy) {
        let mut cmd = Command::new(agy_cli_bin());
        cmd.args(["--resume", session_id, "-p", prompt])
            .env_remove("ANTHROPIC_BASE_URL")
            .env_remove("DISABLE_AUTO_COMPACT");
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
        spawn_detached_cmd(&mut cmd)?;
    } else {
        spawn_claude_detached(&["--resume", session_id, "-p", prompt], dir)?;
    }
    Ok(format!("sent follow-up to {session_id}"))
}

/// Spawn `<agy_route_bin> --prune-brain` detached to clean old agy brain directories.
pub fn prune_agy_brain(agy_route_bin: &str) -> anyhow::Result<String> {
    let bin = if agy_route_bin.trim().is_empty() { "agy-route" } else { agy_route_bin };
    spawn_detached_cmd(Command::new(bin).arg("--prune-brain"))?;
    Ok("pruning old agy brain dirs…".into())
}

fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn as_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " ")
}

/// Desktop notification (macOS via `osascript`, Linux via `notify-send`). Must not block.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification \"{}\" with title \"{}\"", as_escape(body), as_escape(title));
        if let Ok(child) =
            Command::new("osascript").args(["-e", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        {
            reap(child);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(child) =
            Command::new("notify-send").args([title, body]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        {
            reap(child);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body);
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
    fn resume_command_for_switches_on_kind() {
        assert_eq!(resume_command_for(AgentKind::Other, Some("/p/repo"), "ag-1"), "cd '/p/repo' && agy --resume 'ag-1'");
        assert_eq!(resume_command_for(AgentKind::Agy, None, "ag-2"), "agy --resume 'ag-2'");
        assert_eq!(resume_command_for(AgentKind::Claude, Some("/p/repo"), "cc-1"), "cd '/p/repo' && claude --resume 'cc-1'");
    }
    #[test]
    fn killable_agent_cmd_whitelist() {
        assert!(is_killable_agent_cmd("claude", "/opt/homebrew/bin/claude --resume s1"));
        assert!(is_killable_agent_cmd("/opt/homebrew/bin/agy", "agy -p hello"));
        assert!(is_killable_agent_cmd("bun", "/opt/homebrew/bin/bun /Users/u/.claude/bin/agy-route --role worker"));
        assert!(is_killable_agent_cmd("bun", "/opt/homebrew/bin/bun /Users/u/.claude/bin/rosso-auto demo --task hi"));
        assert!(is_killable_agent_cmd("bun", "bun /Users/u/.claude/bin/rosso-run --plan p"));
        assert!(is_killable_agent_cmd("node", "node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"));

        assert!(!is_killable_agent_cmd("office", "/Users/u/bin/office"));
        assert!(!is_killable_agent_cmd("zsh", "/bin/zsh -lic claude"));
        assert!(!is_killable_agent_cmd("launchd", "/sbin/launchd"));
        assert!(!is_killable_agent_cmd(
            "Antigravity Helper",
            "/Applications/Antigravity.app/Contents/Frameworks/Antigravity Helper.app/Contents/MacOS/Antigravity Helper"
        ));
        assert!(!is_killable_agent_cmd("Electron", "/Applications/Antigravity.app/Contents/MacOS/Electron"));
    }
    #[test]
    fn kill_agent_pid_refuses_unsafe_pids() {
        assert!(kill_agent_pid(0).is_err());
        assert!(kill_agent_pid(1).is_err());
        assert!(kill_agent_pid(std::process::id()).is_err());
        #[cfg(unix)]
        assert!(kill_agent_pid(unsafe { libc::getppid() } as u32).is_err());
    }
    #[test]
    fn dispatch_and_followup_validate_inputs() {
        let tmp = std::env::temp_dir();
        let tmp_s = tmp.to_string_lossy();
        assert!(dispatch_task("agy:worker", &tmp_s, "   ", "agy-route").is_err());
        assert!(dispatch_task("agy:manager", &tmp_s, "   ", "agy-route").is_err());
        assert!(dispatch_task("agy:git", "/definitely-not-a-real-dir-xyz", "status", "agy-route").is_err());
        assert!(dispatch_task("rosso-auto:council", &tmp_s, "   ", "agy-route").is_err());
        assert!(dispatch_task("rosso-auto:simple", "/definitely-not-a-real-dir-xyz", "task", "agy-route").is_err());
        assert!(dispatch_task("rosso-auto:bogus", &tmp_s, "task", "agy-route").is_err());
        assert!(dispatch_task("unknown", &tmp_s, "hello", "agy-route").is_err());
        assert!(dispatch_task("claude", "/definitely-not-a-real-dir-xyz", "hello", "agy-route").is_err());
        assert!(followup_session(AgentKind::Claude, None, "", "hi").is_err());
        assert!(followup_session(AgentKind::Other, None, "sid", "  ").is_err());

        // Missing binary fails cleanly after passing target/input validation
        let missing_err = dispatch_task("agy:manager", &tmp_s, "audit", "/nonexistent-agy-route-bin-xyz").unwrap_err();
        assert!(!missing_err.to_string().contains("unknown dispatch target"));
        let git_err = dispatch_task("agy:git", &tmp_s, "diff", "/nonexistent-agy-route-bin-xyz").unwrap_err();
        assert!(!git_err.to_string().contains("unknown dispatch target"));
    }
    #[test]
    fn rosso_auto_controls_validate_inputs() {
        assert!(launch_rosso_auto("bogus", "task", None).is_err());
        assert!(launch_rosso_auto("council", "   ", None).is_err());
        assert!(launch_rosso_auto("simple", "task", Some(Path::new("/definitely-not-a-real-dir-xyz"))).is_err());

        for bad in ["", "-bad", "bad-", "BadJob", "../oops", "a_b", &"a".repeat(49)] {
            assert!(!is_valid_rosso_job(bad), "expected invalid: {bad}");
            assert!(resume_rosso_auto(bad).is_err());
            assert!(abort_rosso_auto(bad).is_err());
        }
        assert!(is_valid_rosso_job("a"));
        assert!(is_valid_rosso_job("auto-fix-login-123"));
        let generated = make_auto_job_name("Fix Auth & Login Bug!!!");
        assert!(is_valid_rosso_job(&generated), "generated invalid job name: {generated}");
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
