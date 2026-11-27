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
    #[cfg(unix)]
    if pid == unsafe { libc::getppid() } as u32 {
        return true;
    }
    pid <= 1 || pid == std::process::id()
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
    let _ = paths::ensure_runtime_dir()?;
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
    paths::atomic_write(&dir.join(&id), if allow { b"allow" } else { b"deny" })?;
    Ok(())
}

/// Validates a Claude/Agy session identifier: non-empty, <= 128 chars, does not start with `'-'`,
/// and contains no control characters, shell metacharacters, quotes, or whitespace.
pub fn is_valid_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 128
        && !s.starts_with('-')
        && s.chars().all(|c| {
            !c.is_control()
                && !c.is_whitespace()
                && !matches!(c, '\'' | '"' | '`' | '$' | ';' | '|' | '&' | '<' | '>')
        })
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

#[cfg(target_os = "macos")]
fn ghostty_exists() -> bool {
    Path::new("/Applications/Ghostty.app").exists()
        || dirs::home_dir().map(|h| h.join("Applications/Ghostty.app").exists()).unwrap_or(false)
}

/// Spawn a new terminal window running `program` with direct `args` (zero shell interpolation).
/// Returns `Ok(true)` if a terminal was launched, or `Ok(false)` if no supported terminal is available.
fn spawn_terminal(cwd: Option<&Path>, program: &str, args: &[&str], strip_anthropic_env: bool) -> anyhow::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        if !ghostty_exists() {
            return Ok(false);
        }
        let mut cmd = Command::new("open");
        cmd.args(["-na", "Ghostty", "--args"]);
        if let Some(d) = cwd.filter(|p| p.is_dir()) {
            cmd.arg(format!("--working-directory={}", d.to_string_lossy()));
        }
        cmd.arg("-e");
        if strip_anthropic_env {
            cmd.args(["/usr/bin/env", "-u", "ANTHROPIC_BASE_URL", "-u", "DISABLE_AUTO_COMPACT"]);
        }
        cmd.arg(program).args(args);
        let child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        reap(child);
        Ok(true)
    }
    #[cfg(target_os = "linux")]
    {
        let valid_dir = cwd.filter(|p| p.is_dir());
        for term in ["ghostty", "kitty", "wezterm", "x-terminal-emulator"] {
            let mut cmd = Command::new(term);
            if strip_anthropic_env {
                cmd.env_remove("ANTHROPIC_BASE_URL").env_remove("DISABLE_AUTO_COMPACT");
            }
            if let Some(d) = valid_dir {
                cmd.current_dir(d);
            }
            match term {
                "ghostty" => {
                    if let Some(d) = valid_dir {
                        cmd.arg(format!("--working-directory={}", d.to_string_lossy()));
                    }
                    cmd.arg("-e").arg(program).args(args);
                }
                "kitty" => {
                    if let Some(d) = valid_dir {
                        cmd.arg("--directory").arg(d);
                    }
                    cmd.arg(program).args(args);
                }
                "wezterm" => {
                    cmd.arg("start");
                    if let Some(d) = valid_dir {
                        cmd.arg("--cwd").arg(d);
                    }
                    cmd.arg("--").arg(program).args(args);
                }
                _ => {
                    cmd.arg("-e").arg(program).args(args);
                }
            }
            match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                Ok(child) => {
                    reap(child);
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => continue,
            }
        }
        Ok(false)
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("wt.exe");
        if strip_anthropic_env {
            cmd.env_remove("ANTHROPIC_BASE_URL").env_remove("DISABLE_AUTO_COMPACT");
        }
        if let Some(d) = cwd.filter(|p| p.is_dir()) {
            cmd.arg("-d").arg(d);
        }
        cmd.arg("--").arg(program).args(args);
        match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(child) => {
                reap(child);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (cwd, program, args, strip_anthropic_env);
        Ok(false)
    }
}

fn launch_resume(cmd_text: String, cwd: Option<&str>, program: &str, session_id: &str, strip_anthropic_env: bool) -> anyhow::Result<String> {
    let _ = crate::app::export::copy_to_clipboard(&cmd_text);
    let dir = cwd.map(str::trim).filter(|c| !c.is_empty()).map(Path::new);
    let _ = spawn_terminal(dir, program, &["--resume", session_id], strip_anthropic_env);
    Ok(cmd_text)
}

/// Resume command for a Claude session: validate `session_id`, copy `cd <cwd> && claude --resume <id>`
/// to the clipboard asynchronously, and try to open a new terminal window running it directly.
pub fn resume_claude(cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(is_valid_session_id(session_id), "invalid session id");
    launch_resume(resume_command(cwd, session_id), cwd, "claude", session_id, false)
}

/// Resume an agent session by kind: validate `session_id`, copy the command to the clipboard,
/// and try to open a new terminal window running it directly.
pub fn resume_agent(kind: AgentKind, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(is_valid_session_id(session_id), "invalid session id");
    let is_agy = matches!(kind, AgentKind::Other | AgentKind::Agy);
    let program = if is_agy { agy_cli_bin() } else { "claude" };
    launch_resume(resume_command_for(kind, cwd, session_id), cwd, program, session_id, is_agy)
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
            anyhow::ensure!(
                spawn_terminal(Some(&dir), "claude", &["--", prompt], false)?,
                "no supported terminal found"
            );
        }
        _ => anyhow::bail!("unknown dispatch target: {target}"),
    }
    Ok(format!("dispatched {target} in {dir_str}"))
}

/// Send a non-interactive follow-up prompt to an existing session.
pub fn followup_session(kind: AgentKind, cwd: Option<&str>, session_id: &str, prompt: &str) -> anyhow::Result<String> {
    anyhow::ensure!(is_valid_session_id(session_id), "invalid session id");
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

/// Escape XML special characters for Windows Toast notification templates.
#[cfg_attr(not(windows), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\r' | '\n' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Encode a UTF-8 string as UTF-16LE Base64 for PowerShell `-EncodedCommand`.
#[cfg_attr(not(windows), allow(dead_code))]
fn encode_utf16le_base64(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64_simd::STANDARD.encode_to_string(&bytes)
}

#[cfg(windows)]
static NOTIFY_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Desktop notification (macOS via `osascript` argv, Linux via `notify-send --`,
/// Windows via single-in-flight hidden PowerShell Toast). Must not block.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(child) = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                "--",
                title,
                body,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            reap(child);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(child) = Command::new("notify-send")
            .args(["--", title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            reap(child);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::sync::atomic::Ordering;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        if NOTIFY_IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let xml_title = xml_escape(title);
        let xml_body = xml_escape(body);
        let ps = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null;\
             $xml = New-Object Windows.Data.Xml.Dom.XmlDocument;\
             $xml.LoadXml('<toast><visual><binding template=\"ToastText02\"><text id=\"1\">{xml_title}</text><text id=\"2\">{xml_body}</text></binding></visual></toast>');\
             $toast = New-Object Windows.UI.Notifications.ToastNotification $xml;\
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('agent-office').Show($toast)"
        );
        let encoded = encode_utf16le_base64(&ps);
        std::thread::spawn(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    NOTIFY_IN_FLIGHT.store(false, Ordering::Release);
                }
            }
            let _guard = Guard;
            if let Ok(mut child) = Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-EncodedCommand",
                    &encoded,
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let _ = child.wait();
            }
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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
        assert!(followup_session(AgentKind::Claude, None, "-flag", "hi").is_err());
        assert!(followup_session(AgentKind::Other, None, "sid;rm", "hi").is_err());
        assert!(followup_session(AgentKind::Other, None, "sid", "  ").is_err());
        assert!(resume_claude(None, "-flag").is_err());
        assert!(resume_agent(AgentKind::Agy, None, "foo;rm").is_err());

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
    fn session_id_validation() {
        for good in ["s1", "conv-123_abc.4", "550e8400-e29b-41d4-a716-446655440000", &"a".repeat(128)] {
            assert!(is_valid_session_id(good), "expected valid: {good}");
        }
        for bad in [
            "",
            "-flag",
            "--resume",
            "foo;rm",
            "a'b",
            "a\"b",
            "a`b",
            "a$b",
            "a|b",
            "a&b",
            "a<b",
            "a>b",
            "a b",
            "a\nb",
            "a\x00b",
            "a\x1bb",
            &"a".repeat(129),
        ] {
            assert!(!is_valid_session_id(bad), "expected invalid: {bad:?}");
        }
    }
    #[test]
    fn xml_escape_handles_special_chars() {
        assert_eq!(
            xml_escape("A & <B> \"C\" 'D'\nE\x07F"),
            "A &amp; &lt;B&gt; &quot;C&quot; &apos;D&apos; EF"
        );
    }
    #[test]
    fn utf16le_base64_matches_powershell_format() {
        // "hi" in UTF-16LE is [0x68, 0x00, 0x69, 0x00] -> Base64 "aABpAA=="
        assert_eq!(encode_utf16le_base64("hi"), "aABpAA==");
        assert_eq!(encode_utf16le_base64(""), "");
    }
    #[test]
    fn cancel_refuses_init() {
        assert!(cancel_agy(1).is_err());
        assert!(cancel_agy(0).is_err());
    }
}
