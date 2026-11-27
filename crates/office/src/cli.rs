//! Workstream A. `office install-hooks [--claude] [--antigravity] [--uninstall] [--permission]`,
//! `office import ...`, `office desktop [--demo]`, `office doctor`.
//!
//! Paths are overridable for testing: `OFFICE_CLAUDE_SETTINGS` (default ~/.claude/settings.json) and
//! `OFFICE_GEMINI_PLUGINS` (default ~/.gemini/config/plugins).
use office_core::{assets, config::Config, paths, whiteboard};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Claude Code events we hook (PermissionRequest is added separately, only when enabled).
pub const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
    "StopFailure",
];
const AG_EVENTS: &[&str] = &["SessionStart", "PreInvocation", "PostInvocation", "PreToolUse", "PostToolUse", "Stop"];
const AG_PLUGIN: &str = "agent-office";
/// Every command we install contains this marker; uninstall removes exactly those.
const MARKER: &str = "OFFICE_SOURCE=";

pub fn claude_settings_path() -> PathBuf {
    std::env::var("OFFICE_CLAUDE_SETTINGS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::claude_dir().join("settings.json"))
}

pub fn gemini_plugins_dir() -> PathBuf {
    std::env::var("OFFICE_GEMINI_PLUGINS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".gemini/config/plugins"))
}

/// office-hook binary name for the current platform.
fn hook_bin_name() -> &'static str {
    if cfg!(windows) {
        "office-hook.exe"
    } else {
        "office-hook"
    }
}

/// office-hook next to the running `office`, then ~/.cargo/bin, then PATH.
pub fn find_office_hook() -> Option<PathBuf> {
    let bin = hook_bin_name();
    let next = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(bin)));
    let cargo = dirs::home_dir().map(|h| h.join(".cargo").join("bin").join(bin));
    if let Some(p) = [next, cargo].into_iter().flatten().find(|p| p.is_file()) {
        return Some(p);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// office-desktop binary name for the current platform.
fn desktop_bin_name() -> &'static str {
    if cfg!(windows) {
        "office-desktop.exe"
    } else {
        "office-desktop"
    }
}

/// SEC-7: resolve `office-desktop` exclusively from the canonical parent directory of `exe`.
/// Never falls back to `PATH` or CWD, and refuses symlinks / non-regular files.
fn find_office_desktop_next_to(exe: &Path) -> Option<PathBuf> {
    let cand = std::fs::canonicalize(exe).ok()?.parent()?.join(desktop_bin_name());
    let meta = std::fs::symlink_metadata(&cand).ok()?;
    if !meta.file_type().is_symlink() && meta.file_type().is_file() {
        Some(cand)
    } else {
        None
    }
}

/// Resolve `office-desktop` strictly beside the canonical running `office` executable (SEC-7).
pub fn find_office_desktop() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    find_office_desktop_next_to(&exe)
}

pub fn launch_desktop(demo: bool) -> anyhow::Result<()> {
    let bin = find_office_desktop().ok_or_else(|| {
        anyhow::anyhow!(
            "office-desktop not installed next to office (install `office-desktop` alongside `office`)"
        )
    })?;
    let mut cmd = std::process::Command::new(&bin);
    if demo {
        cmd.arg("--demo");
    }
    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("failed to launch {}: {e}", bin.display()))?;
    if !status.success() {
        match status.code() {
            Some(code) => anyhow::bail!("office-desktop exited with status {code}"),
            None => anyhow::bail!("office-desktop terminated by signal"),
        }
    }
    Ok(())
}

const IMPORT_USAGE: &str = "\
usage:
  office import theme <dir> <name>
  office import sprite <theme> <sprite> <png>
  office import ceo-outfit <png-or-dir> <name> [--mode torso|full]
  office import sound <test|needs-you|inbox|error> <wav>";

#[derive(Debug, PartialEq, Eq)]
enum ImportCmd<'a> {
    Theme { dir: &'a str, name: &'a str },
    Sprite { theme: &'a str, sprite: &'a str, png: &'a str },
    CeoOutfit { src: &'a str, name: &'a str, mode: Option<&'a str> },
    Sound { slot: &'a str, wav: &'a str },
}

fn parse_import_args(args: &[String]) -> anyhow::Result<ImportCmd<'_>> {
    let Some((sub, rest)) = args.split_first() else {
        anyhow::bail!("missing import subcommand\n{IMPORT_USAGE}");
    };
    match sub.as_str() {
        "theme" => {
            if rest.len() != 2 {
                anyhow::bail!("usage: office import theme <dir> <name>");
            }
            Ok(ImportCmd::Theme { dir: &rest[0], name: &rest[1] })
        }
        "sprite" => {
            if rest.len() != 3 {
                anyhow::bail!("usage: office import sprite <theme> <sprite> <png>");
            }
            Ok(ImportCmd::Sprite { theme: &rest[0], sprite: &rest[1], png: &rest[2] })
        }
        "ceo-outfit" => {
            let mut positional: Vec<&str> = Vec::new();
            let mut mode: Option<&str> = None;
            let mut i = 0;
            while i < rest.len() {
                let arg = rest[i].as_str();
                if arg == "--mode" {
                    let Some(val) = rest.get(i + 1) else {
                        anyhow::bail!(
                            "--mode requires a value (torso or full)\nusage: office import ceo-outfit <png-or-dir> <name> [--mode torso|full]"
                        );
                    };
                    if mode.is_some() {
                        anyhow::bail!("--mode specified more than once");
                    }
                    mode = Some(val.as_str());
                    i += 2;
                } else if let Some(val) = arg.strip_prefix("--mode=") {
                    if mode.is_some() {
                        anyhow::bail!("--mode specified more than once");
                    }
                    mode = Some(val);
                    i += 1;
                } else if arg.starts_with('-') {
                    anyhow::bail!(
                        "unknown option {arg:?}\nusage: office import ceo-outfit <png-or-dir> <name> [--mode torso|full]"
                    );
                } else {
                    positional.push(arg);
                    i += 1;
                }
            }
            if positional.len() != 2 {
                anyhow::bail!("usage: office import ceo-outfit <png-or-dir> <name> [--mode torso|full]");
            }
            if let Some(m) = mode {
                if m != "torso" && m != "full" {
                    anyhow::bail!("invalid outfit mode {m:?}: expected \"torso\" or \"full\"");
                }
            }
            Ok(ImportCmd::CeoOutfit { src: positional[0], name: positional[1], mode })
        }
        "sound" => {
            if rest.len() != 2 {
                anyhow::bail!("usage: office import sound <test|needs-you|inbox|error> <wav>");
            }
            Ok(ImportCmd::Sound { slot: &rest[0], wav: &rest[1] })
        }
        other => anyhow::bail!("unknown import subcommand {other:?}\n{IMPORT_USAGE}"),
    }
}

pub fn import_asset(args: &[String]) -> anyhow::Result<()> {
    match parse_import_args(args)? {
        ImportCmd::Theme { dir, name } => {
            let dest = assets::import_theme_pack(Path::new(dir), name).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("imported theme {name} -> {}", dest.display());
        }
        ImportCmd::Sprite { theme, sprite, png } => {
            let dest = assets::import_sprite_override(theme, sprite, Path::new(png)).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("imported sprite {theme}/{sprite} -> {}", dest.display());
        }
        ImportCmd::CeoOutfit { src, name, mode } => {
            let dest = assets::import_ceo_outfit(Path::new(src), name, mode).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("imported ceo-outfit {name} -> {}", dest.display());
        }
        ImportCmd::Sound { slot, wav } => {
            let dest = assets::import_sound(slot, Path::new(wav)).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("imported sound {slot} -> {}", dest.display());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn hook_command(source: &str, hook: &Path, extra: &str) -> String {
    #[cfg(unix)]
    {
        format!("OFFICE_SOURCE={source} {}{extra}", sh_quote(&hook.to_string_lossy()))
    }
    #[cfg(windows)]
    {
        format!("\"{}\" --source {source}{extra}", hook.to_string_lossy())
    }
}

fn is_ours(cmd: &str) -> bool {
    (cmd.contains(MARKER) || cmd.contains("--source ")) && cmd.contains("office-hook")
}

pub fn install_hooks(claude: bool, antigravity: bool, uninstall: bool) -> anyhow::Result<()> {
    let cfg = Config::load();
    let permission = std::env::args().any(|a| a == "--permission") || cfg.permission_timeout_s > 0;
    let hook = if uninstall {
        None
    } else {
        Some(find_office_hook().ok_or_else(|| {
            anyhow::anyhow!("office-hook not found next to this binary or in ~/.cargo/bin (cargo install --path crates/office-hook)")
        })?)
    };
    if claude {
        let path = claude_settings_path();
        let perm = permission && cfg.permission_timeout_s > 0;
        let changed = update_claude_settings(&path, hook.as_deref(), perm, cfg.permission_timeout_s)?;
        println!("claude: {} {}", if changed { "updated" } else { "unchanged" }, path.display());
        if permission && cfg.permission_timeout_s == 0 && !uninstall {
            println!("claude: PermissionRequest not installed (set permission_timeout_s > 0 in {})", paths::config_file().display());
        }
    }
    if antigravity {
        let dir = gemini_plugins_dir().join(AG_PLUGIN);
        match &hook {
            None => {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)?;
                    println!("antigravity: removed {}", dir.display());
                } else {
                    println!("antigravity: nothing to remove");
                }
            }
            Some(h) => {
                write_antigravity_plugin(&dir, h)?;
                println!("antigravity: wrote {}", dir.display());
            }
        }
    }
    Ok(())
}

fn hook_entry(cmd: String, timeout: u64) -> Value {
    json!({ "hooks": [ { "type": "command", "command": cmd, "timeout": timeout } ] })
}

/// Merge (or with `hook == None`, remove) our entries in a Claude settings.json. Returns true if written.
/// Idempotent: our old entries are removed first, then re-added; other hooks are untouched.
pub fn update_claude_settings(path: &Path, hook: Option<&Path>, permission: bool, permission_timeout_s: u64) -> anyhow::Result<bool> {
    let original = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => anyhow::bail!("cannot read {}: {e}", path.display()),
    };
    let mut root: Value = match &original {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(s).map_err(|e| anyhow::anyhow!("{}: invalid JSON: {e}", path.display()))?,
        _ => json!({}),
    };
    let obj = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("{}: not a JSON object", path.display()))?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings.hooks is not an object"))?;

    strip_ours(hooks);
    if let Some(h) = hook {
        let base = hook_command("claude", h, "");
        for ev in CLAUDE_EVENTS {
            push_group(hooks, ev, hook_entry(base.clone(), 5));
        }
        if permission {
            push_group(
                hooks,
                "PermissionRequest",
                hook_entry(hook_command("claude", h, " --permission"), permission_timeout_s + 5),
            );
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    let text = serde_json::to_string_pretty(&root)? + "\n";
    if original.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let orig_perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    if let Some(orig) = &original {
        let prefix = format!("{}.bak-office-", path.file_name().and_then(|s| s.to_str()).unwrap_or("settings.json"));
        if let Some(dir) = path.parent() {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&prefix) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
        let bak = path.with_file_name(format!("{prefix}{}", paths::now_ms() / 1000));
        std::fs::write(&bak, orig)?;
    }
    if let Some(d) = target.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = target.with_extension("json.office-tmp");
    std::fs::write(&tmp, &text)?;
    if let Some(perms) = orig_perms {
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    std::fs::rename(&tmp, &target)?;
    Ok(true)
}

fn push_group(hooks: &mut Map<String, Value>, ev: &str, group: Value) {
    let arr = hooks.entry(ev.to_string()).or_insert_with(|| json!([]));
    if !arr.is_array() {
        *arr = json!([]);
    }
    arr.as_array_mut().expect("array").push(group);
}

/// Remove every hook command of ours; drop groups/events that become empty.
fn strip_ours(hooks: &mut Map<String, Value>) {
    let mut empty_events = Vec::new();
    for (ev, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else { continue };
        groups.retain_mut(|g| {
            let Some(inner) = g.get_mut("hooks").and_then(Value::as_array_mut) else { return true };
            let before = inner.len();
            inner.retain(|h| !h.get("command").and_then(Value::as_str).map(is_ours).unwrap_or(false));
            !(inner.is_empty() && before > 0)
        });
        if groups.is_empty() {
            empty_events.push(ev.clone());
        }
    }
    for ev in empty_events {
        hooks.remove(&ev);
    }
}

/// ~/.gemini/config/plugins/agent-office/{plugin.json,hooks.json}, same shape as other plugins (e.g. pixtuoid).
pub fn write_antigravity_plugin(dir: &Path, hook: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let plugin = json!({ "name": AG_PLUGIN, "description": "Sends Antigravity CLI's session events to agent-office" });
    let mut events = Map::new();
    for ev in AG_EVENTS {
        let cmd = json!({
            "type": "command",
            "command": hook_command("antigravity", hook, &format!(" --hook-event {ev}"))
        });
        let v = if ev.ends_with("ToolUse") { json!([{ "matcher": "*", "hooks": [cmd] }]) } else { json!([cmd]) };
        events.insert(ev.to_string(), v);
    }
    let hooks = json!({ AG_PLUGIN: Value::Object(events) });
    write_private(&dir.join("plugin.json"), &(serde_json::to_string_pretty(&plugin)? + "\n"))?;
    write_private(&dir.join("hooks.json"), &(serde_json::to_string_pretty(&hooks)? + "\n"))?;
    Ok(())
}

fn write_private(p: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(p, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn extract_hook_bin(cmd: &str) -> Option<&str> {
    let needle = if cfg!(windows) && cmd.contains("office-hook.exe") {
        "office-hook.exe"
    } else {
        "office-hook"
    };
    let idx = cmd.find(needle)?;
    let rest = &cmd[..idx + needle.len()];
    let start = rest
        .rfind(['\'', '"'])
        .map(|i| i + 1)
        .unwrap_or_else(|| rest.rfind(' ').map(|i| i + 1).unwrap_or(0));
    Some(&rest[start..])
}

fn count_ours(settings: &Value) -> (usize, Vec<String>, Vec<String>) {
    let mut n = 0;
    let mut evs = Vec::new();
    let mut missing_bins = Vec::new();
    if let Some(h) = settings.get("hooks").and_then(Value::as_object) {
        for (ev, groups) in h {
            let mut c = 0;
            for cmd in groups
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g.get("hooks").and_then(Value::as_array).into_iter().flatten())
                .filter_map(|x| x.get("command").and_then(Value::as_str))
                .filter(|cmd| is_ours(cmd))
            {
                c += 1;
                if let Some(bin) = extract_hook_bin(cmd) {
                    if !Path::new(bin).is_file() && !missing_bins.iter().any(|b| b == bin) {
                        missing_bins.push(bin.to_string());
                    }
                }
            }
            if c > 0 {
                n += c;
                evs.push(ev.clone());
            }
        }
    }
    (n, evs, missing_bins)
}

pub fn doctor() -> anyhow::Result<()> {
    let cfg = Config::load();
    let ok = |b: bool| if b { "ok " } else { "-- " };
    println!("ROSSO office doctor");
    let cf = paths::config_file();
    println!("{}config       {}{}", ok(cf.exists()), cf.display(), if cf.exists() { "" } else { " (defaults)" });

    let sock = paths::hook_socket();
    let state = {
        #[cfg(unix)]
        {
            if std::fs::symlink_metadata(&sock).is_err() {
                "absent (office not running)"
            } else if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
                "listening"
            } else {
                "stale (no listener)"
            }
        }
        #[cfg(windows)]
        {
            match std::fs::read_to_string(&sock) {
                Err(_) => "absent (office not running)",
                Ok(raw) => match raw.trim().parse::<std::net::SocketAddr>() {
                    Ok(addr)
                        if addr.ip().is_loopback()
                            && std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_ok() =>
                    {
                        "listening"
                    }
                    _ => "stale (no listener)",
                },
            }
        }
    };
    println!("{}socket       {} [{state}]", ok(state == "listening"), sock.display());

    let tdirs = crate::render::theme::search_dirs();
    let themes = crate::render::theme::list(&tdirs);
    let bundled = themes.len().saturating_sub(1);
    println!("{}themes       {bundled} packs ({})", ok(bundled > 0), themes.join(", "));

    match find_office_hook() {
        Some(h) => println!("ok office-hook  {}", h.display()),
        None => println!("-- office-hook  not found (cargo install --path crates/office-hook)"),
    }

    match find_office_desktop() {
        Some(p) => println!("ok office-desktop {}", p.display()),
        None => println!("-- office-desktop not installed next to office"),
    }

    let (audio_ok, audio_backend) = detect_audio_backend();
    let custom_wavs = count_custom_wavs(&paths::sounds_dir());
    println!(
        "{}audio        {audio_backend} [mode {}, {custom_wavs} custom WAV(s), sound {}]",
        ok(audio_ok),
        cfg.sounds.mode,
        if cfg.sound { "on" } else { "off" }
    );

    let outfits = assets::list_outfits();
    let active_outfit = cfg.ceo.outfit.as_deref().unwrap_or("none");
    let outfit_ok = match cfg.ceo.outfit.as_deref() {
        Some(name) => outfits.iter().any(|o| o == name),
        None => true,
    };
    let outfit_list = if outfits.is_empty() {
        String::new()
    } else {
        format!(" ({})", outfits.join(", "))
    };
    println!(
        "{}outfits      {} installed{outfit_list} [active: {active_outfit}, mode: {}]",
        ok(outfit_ok),
        outfits.len(),
        cfg.ceo.outfit_mode
    );

    let wb_path = paths::whiteboard_file();
    match std::fs::symlink_metadata(&wb_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("-- whiteboard   {} [empty/absent]", wb_path.display());
        }
        Err(e) => {
            println!("-- whiteboard   {} [unreadable: {e}]", wb_path.display());
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            println!("-- whiteboard   {} [refusing symlink]", wb_path.display());
        }
        Ok(meta) if meta.len() > whiteboard::MAX_WHITEBOARD_BYTES as u64 => {
            println!("-- whiteboard   {} [exceeds size limit]", wb_path.display());
        }
        Ok(_) => match std::fs::read(&wb_path) {
            Err(e) => println!("-- whiteboard   {} [unreadable: {e}]", wb_path.display()),
            Ok(bytes) => match serde_json::from_slice::<whiteboard::Whiteboard>(&bytes) {
                Err(e) => println!("-- whiteboard   {} [invalid JSON: {e}]", wb_path.display()),
                Ok(wb) => {
                    let (done, total) = wb.counts();
                    if total == 0 {
                        println!("ok whiteboard   {} [empty/absent]", wb_path.display());
                    } else {
                        println!(
                            "ok whiteboard   {} [{done}/{total} items, {}%]",
                            wb_path.display(),
                            wb.completion_pct()
                        );
                    }
                }
            },
        },
    }

    let sp = claude_settings_path();
    match std::fs::read_to_string(&sp) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("-- claude hooks {} missing", sp.display()),
        Err(e) => println!("-- claude hooks {} unreadable: {e}", sp.display()),
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Err(e) => println!("-- claude hooks {} invalid JSON: {e}", sp.display()),
            Ok(v) => {
                let (n, evs, missing_bins) = count_ours(&v);
                let missing: Vec<&str> = CLAUDE_EVENTS.iter().copied().filter(|e| !evs.iter().any(|x| x == e)).collect();
                let bin_note = if missing_bins.is_empty() {
                    String::new()
                } else {
                    format!(", missing hook binary: {}", missing_bins.join(", "))
                };
                println!(
                    "{}claude hooks {} [{n} entries{}{}{}]",
                    ok(missing.is_empty() && missing_bins.is_empty()),
                    sp.display(),
                    if missing.is_empty() { String::new() } else { format!(", missing: {}", missing.join(",")) },
                    if evs.iter().any(|e| e == "PermissionRequest") { ", PermissionRequest on" } else { "" },
                    bin_note
                );
            }
        },
    }
    let ag = gemini_plugins_dir().join(AG_PLUGIN).join("hooks.json");
    match std::fs::read_to_string(&ag) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("-- antigravity  {} missing", ag.display()),
        Err(e) => println!("-- antigravity  {} unreadable: {e}", ag.display()),
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Err(e) => println!("-- antigravity  {} invalid JSON: {e}", ag.display()),
            Ok(v) => {
                let events = v.get(AG_PLUGIN).and_then(Value::as_object);
                let present = events.map(|m| m.len()).unwrap_or(0);
                let missing: Vec<&str> = AG_EVENTS
                    .iter()
                    .copied()
                    .filter(|e| !events.is_some_and(|m| m.contains_key(*e)))
                    .collect();
                println!(
                    "{}antigravity  {} [{present}/{} events{}]",
                    ok(missing.is_empty()),
                    ag.display(),
                    AG_EVENTS.len(),
                    if missing.is_empty() { String::new() } else { format!(", missing: {}", missing.join(",")) }
                );
            }
        },
    }

    let route = Path::new(&cfg.agy_route);
    if route.exists() {
        match crate::sources::agy_router::run_dry(&cfg.agy_route) {
            Some(text) => {
                let st = crate::sources::agy_router::parse_dry_run(&text);
                let v = |o: Option<u32>| o.map_or_else(|| "?".to_string(), |n| n.to_string());
                let pressure = match st.pressure {
                    Some(1) => "normal".to_string(),
                    Some(2) => "warn".to_string(),
                    Some(4) => "critical".to_string(),
                    other => v(other.map(u32::from)),
                };
                println!(
                    "{}agy-route    {} [capacity {}, slots {}/{}, free {} MB, pressure {}]",
                    ok(st.capacity.is_some()),
                    route.display(),
                    v(st.capacity),
                    st.busy_slots,
                    v(st.max_concurrent),
                    v(st.free_mb),
                    pressure
                );
            }
            None => println!("-- agy-route    {} status check timed out or failed", route.display()),
        }
    } else {
        println!("-- agy-route    {} not found", route.display());
    }

    let tdir = paths::claude_dir().join("projects");
    let recent = count_recent_jsonl(&tdir, 0);
    println!("{}transcripts  {} [{recent} modified in the last 2h]", ok(tdir.exists()), tdir.display());
    println!("   permission   timeout {} s ({})", cfg.permission_timeout_s, if cfg.permission_timeout_s > 0 { "approve/deny on" } else { "off" });
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn detect_audio_backend() -> (bool, String) {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/afplay").is_file() || find_in_path("afplay").is_some() {
            (true, "afplay".into())
        } else {
            (false, "afplay not found".into())
        }
    }
    #[cfg(target_os = "linux")]
    {
        const PLAYERS: [&str; 4] = ["paplay", "pw-play", "aplay", "canberra-gtk-play"];
        if let Some(found) = PLAYERS.iter().copied().find(|p| find_in_path(p).is_some()) {
            (true, found.into())
        } else {
            (false, format!("none (tried {})", PLAYERS.join(", ")))
        }
    }
    #[cfg(windows)]
    {
        (true, "Win32 PlaySoundW".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        (false, "unsupported platform".into())
    }
}

fn count_custom_wavs(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            continue;
        }
        let is_wav = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
        if is_wav {
            n += 1;
        }
    }
    n
}

fn count_recent_jsonl(dir: &Path, depth: u32) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 3 {
            n += count_recent_jsonl(&p, depth + 1);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            let fresh = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|d| d.as_secs() < 7200)
                .unwrap_or(false);
            n += fresh as usize;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("office-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_is_idempotent_preserves_others_and_uninstalls() {
        let d = tmpdir("claude");
        let p = d.join("settings.json");
        let orig = json!({
            "model": "opus",
            "hooks": {
                "Stop": [ { "matcher": "", "hooks": [ { "type": "command", "command": "pixtuoid-hook Stop" } ] } ],
                "PreToolUse": [ { "hooks": [ { "type": "command", "command": "node agent-viz.js hook --source=claude" } ] } ]
            }
        });
        std::fs::write(&p, serde_json::to_string_pretty(&orig).unwrap()).unwrap();
        let hook = Path::new("/opt/x/office-hook");
        assert!(update_claude_settings(&p, Some(hook), false, 0).unwrap());
        let backups = std::fs::read_dir(&d).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".bak-office-")).count();
        assert_eq!(backups, 1);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(count_ours(&v).0, CLAUDE_EVENTS.len());
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "pixtuoid-hook Stop");
        assert_eq!(v["hooks"]["PreToolUse"][1]["hooks"][0]["command"], hook_command("claude", hook, ""));
        assert!(v["hooks"].get("PermissionRequest").is_none());
        // idempotent
        assert!(!update_claude_settings(&p, Some(hook), false, 0).unwrap());
        // permission entry
        assert!(update_claude_settings(&p, Some(hook), true, 30).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], 35);
        assert_eq!(count_ours(&v).0, CLAUDE_EVENTS.len() + 1);
        // uninstall restores the original hooks
        assert!(update_claude_settings(&p, None, false, 0).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v, orig);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn antigravity_plugin_shape() {
        let d = tmpdir("ag");
        let hook = Path::new("/x/office-hook");
        write_antigravity_plugin(&d.join(AG_PLUGIN), hook).unwrap();
        let h: Value = serde_json::from_str(&std::fs::read_to_string(d.join(AG_PLUGIN).join("hooks.json")).unwrap()).unwrap();
        let events = h[AG_PLUGIN].as_object().unwrap();
        assert_eq!(events.len(), AG_EVENTS.len());
        for ev in ["SessionStart", "PreInvocation", "PostInvocation", "Stop"] {
            assert_eq!(
                h[AG_PLUGIN][ev][0]["command"],
                hook_command("antigravity", hook, &format!(" --hook-event {ev}"))
            );
        }
        for ev in ["PreToolUse", "PostToolUse"] {
            assert_eq!(h[AG_PLUGIN][ev][0]["matcher"], "*");
            assert_eq!(
                h[AG_PLUGIN][ev][0]["hooks"][0]["command"],
                hook_command("antigravity", hook, &format!(" --hook-event {ev}"))
            );
        }
        let p: Value = serde_json::from_str(&std::fs::read_to_string(d.join(AG_PLUGIN).join("plugin.json")).unwrap()).unwrap();
        assert_eq!(p["name"], AG_PLUGIN);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    fn minimal_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(33);
        out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn minimal_wav() -> Vec<u8> {
        let mut out = Vec::with_capacity(44);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&36u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&[0u8; 32]);
        out
    }

    fn import_asset_in(
        args: &[String],
        themes_root: &Path,
        outfits_root: &Path,
        sounds_root: &Path,
    ) -> anyhow::Result<PathBuf> {
        match parse_import_args(args)? {
            ImportCmd::Theme { dir, name } => {
                assets::import_theme_pack_to(Path::new(dir), name, themes_root).map_err(|e| anyhow::anyhow!("{e}"))
            }
            ImportCmd::Sprite { theme, sprite, png } => {
                assets::import_sprite_override_to(theme, sprite, Path::new(png), themes_root).map_err(|e| anyhow::anyhow!("{e}"))
            }
            ImportCmd::CeoOutfit { src, name, mode } => {
                assets::import_ceo_outfit_to(Path::new(src), name, mode, outfits_root).map_err(|e| anyhow::anyhow!("{e}"))
            }
            ImportCmd::Sound { slot, wav } => {
                assets::import_sound_to(slot, Path::new(wav), sounds_root).map_err(|e| anyhow::anyhow!("{e}"))
            }
        }
    }

    #[test]
    fn import_asset_rejects_bad_arguments_and_unknown_subcommands() {
        assert!(import_asset(&[]).is_err());
        assert!(import_asset(&s(&["bogus"])).is_err());
        assert!(import_asset(&s(&["theme"])).is_err());
        assert!(import_asset(&s(&["theme", "/only-one"])).is_err());
        assert!(import_asset(&s(&["theme", "a", "b", "extra"])).is_err());
        assert!(import_asset(&s(&["sprite", "cozy", "desk"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png", "suit", "--mode"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png", "suit", "--mode", "cape"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png", "suit", "--mode=cape"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png", "suit", "--mode", "torso", "--mode", "full"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "shirt.png", "suit", "--unknown"])).is_err());
        assert!(import_asset(&s(&["sound", "test"])).is_err());
        assert!(import_asset(&s(&["sound", "bad-slot", "/nonexistent.wav"])).is_err());
        // Pack name traversal fails validation before touching user config.
        assert!(import_asset(&s(&["theme", "/nonexistent", "../evil"])).is_err());
        assert!(import_asset(&s(&["sprite", "../evil", "desk", "/nonexistent.png"])).is_err());
        assert!(import_asset(&s(&["ceo-outfit", "/nonexistent.png", "../evil"])).is_err());
    }

    #[test]
    fn import_asset_parses_and_imports_all_four_targets_end_to_end() {
        let d = tmpdir("import-e2e");
        let themes_root = d.join("themes");
        let outfits_root = d.join("outfits");
        let sounds_root = d.join("sounds");

        // 1. theme
        let src_theme = d.join("src_theme");
        std::fs::create_dir_all(&src_theme).unwrap();
        std::fs::write(src_theme.join("sheet.png"), minimal_png(32, 32)).unwrap();
        std::fs::write(
            src_theme.join("theme.toml"),
            "[sheets]\ns = { file = \"sheet.png\", tile = 16 }\n[sprites]\nfloor = { sheet = \"s\", col = 0, row = 0 }\n",
        )
        .unwrap();
        let theme_dest = import_asset_in(
            &s(&["theme", src_theme.to_str().unwrap(), "my-theme"]),
            &themes_root,
            &outfits_root,
            &sounds_root,
        )
        .unwrap();
        assert_eq!(theme_dest, themes_root.join("my-theme"));
        assert!(theme_dest.join("theme.toml").is_file());

        // 2. sprite
        let src_sprite = d.join("desk.png");
        std::fs::write(&src_sprite, minimal_png(16, 16)).unwrap();
        let sprite_dest = import_asset_in(
            &s(&["sprite", "my-theme", "desk", src_sprite.to_str().unwrap()]),
            &themes_root,
            &outfits_root,
            &sounds_root,
        )
        .unwrap();
        assert_eq!(sprite_dest, themes_root.join("my-theme/sprites/desk.png"));
        assert!(sprite_dest.is_file());

        // 3. ceo-outfit (with --mode full and --mode=torso)
        let src_outfit = d.join("tux.png");
        std::fs::write(&src_outfit, minimal_png(16, 24)).unwrap();
        let outfit_dest = import_asset_in(
            &s(&["ceo-outfit", src_outfit.to_str().unwrap(), "tuxedo", "--mode", "full"]),
            &themes_root,
            &outfits_root,
            &sounds_root,
        )
        .unwrap();
        assert_eq!(outfit_dest, outfits_root.join("tuxedo"));
        let manifest = std::fs::read_to_string(outfit_dest.join("outfit.toml")).unwrap();
        assert!(manifest.contains("mode = \"full\""));

        let outfit_dest2 = import_asset_in(
            &s(&["ceo-outfit", "--mode=torso", src_outfit.to_str().unwrap(), "jersey"]),
            &themes_root,
            &outfits_root,
            &sounds_root,
        )
        .unwrap();
        assert_eq!(outfit_dest2, outfits_root.join("jersey"));
        let manifest2 = std::fs::read_to_string(outfit_dest2.join("outfit.toml")).unwrap();
        assert!(manifest2.contains("mode = \"torso\""));

        // 4. sound
        let src_wav = d.join("ping.wav");
        std::fs::write(&src_wav, minimal_wav()).unwrap();
        let sound_dest = import_asset_in(
            &s(&["sound", "needs-you", src_wav.to_str().unwrap()]),
            &themes_root,
            &outfits_root,
            &sounds_root,
        )
        .unwrap();
        assert_eq!(sound_dest, sounds_root.join("needs-you.wav"));
        assert!(sound_dest.is_file());
        assert_eq!(count_custom_wavs(&sounds_root), 1);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn find_office_desktop_enforces_sec7_sibling_and_rejects_symlinks_and_dirs() {
        let d = tmpdir("desktop-sec7");
        let exe = d.join("office");
        std::fs::write(&exe, b"fake-office").unwrap();

        // Absent -> None
        assert_eq!(find_office_desktop_next_to(&exe), None);

        // Directory with binary name -> rejected
        let cand = d.join(desktop_bin_name());
        std::fs::create_dir(&cand).unwrap();
        assert_eq!(find_office_desktop_next_to(&exe), None);
        std::fs::remove_dir(&cand).unwrap();

        // Regular file next to exe -> accepted (canonical path)
        std::fs::write(&cand, b"fake-desktop").unwrap();
        let expected = std::fs::canonicalize(&exe).unwrap().parent().unwrap().join(desktop_bin_name());
        assert_eq!(find_office_desktop_next_to(&exe), Some(expected.clone()));

        // Unix symlink checks:
        // 1. Symlinked office binary resolves to real parent dir (finds real sibling)
        // 2. Symlinked office-desktop candidate is rejected
        #[cfg(unix)]
        {
            let link_dir = tmpdir("desktop-sec7-link");
            let exe_link = link_dir.join("office");
            std::os::unix::fs::symlink(&exe, &exe_link).unwrap();
            assert_eq!(find_office_desktop_next_to(&exe_link), Some(expected));

            let real_target = link_dir.join("real-desktop");
            std::fs::write(&real_target, b"real").unwrap();
            std::fs::remove_file(&cand).unwrap();
            std::os::unix::fs::symlink(&real_target, &cand).unwrap();
            assert_eq!(find_office_desktop_next_to(&exe), None);

            let _ = std::fs::remove_dir_all(&link_dir);
        }

        let _ = std::fs::remove_dir_all(&d);
    }
}
