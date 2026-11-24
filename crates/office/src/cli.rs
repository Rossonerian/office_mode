//! Workstream A. `office install-hooks [--claude] [--antigravity] [--uninstall] [--permission]`, `office doctor`.
//!
//! Paths are overridable for testing: `OFFICE_CLAUDE_SETTINGS` (default ~/.claude/settings.json) and
//! `OFFICE_GEMINI_PLUGINS` (default ~/.gemini/config/plugins).
use office_core::{config::Config, paths};
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
];
const AG_EVENTS: &[&str] = &["SessionStart", "PreToolUse", "PostToolUse", "Stop"];
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

/// office-hook next to the running `office`, then ~/.cargo/bin.
pub fn find_office_hook() -> Option<PathBuf> {
    let next = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("office-hook")));
    let cargo = dirs::home_dir().map(|h| h.join(".cargo/bin/office-hook"));
    [next, cargo].into_iter().flatten().find(|p| p.is_file())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn is_ours(cmd: &str) -> bool {
    cmd.contains(MARKER) && cmd.contains("office-hook")
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
    let original = std::fs::read_to_string(path).ok();
    let mut root: Value = match &original {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(s).map_err(|e| anyhow::anyhow!("{}: invalid JSON: {e}", path.display()))?,
        _ => json!({}),
    };
    let obj = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("{}: not a JSON object", path.display()))?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings.hooks is not an object"))?;

    strip_ours(hooks);
    if let Some(h) = hook {
        let base = format!("OFFICE_SOURCE=claude {}", sh_quote(&h.to_string_lossy()));
        for ev in CLAUDE_EVENTS {
            push_group(hooks, ev, hook_entry(base.clone(), 5));
        }
        if permission {
            push_group(hooks, "PermissionRequest", hook_entry(format!("{base} --permission"), permission_timeout_s + 5));
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    let text = serde_json::to_string_pretty(&root)? + "\n";
    if original.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    if let Some(orig) = &original {
        let bak = path.with_file_name(format!(
            "{}.bak-office-{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("settings.json"),
            paths::now_ms() / 1000
        ));
        std::fs::write(&bak, orig)?;
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.office-tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, path)?;
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
            "command": format!("OFFICE_SOURCE=antigravity {} --hook-event {ev}", sh_quote(&hook.to_string_lossy()))
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
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, text)?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn count_ours(settings: &Value) -> (usize, Vec<String>) {
    let mut n = 0;
    let mut evs = Vec::new();
    if let Some(h) = settings.get("hooks").and_then(Value::as_object) {
        for (ev, groups) in h {
            let c = groups
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g.get("hooks").and_then(Value::as_array).into_iter().flatten())
                .filter(|x| x.get("command").and_then(Value::as_str).map(is_ours).unwrap_or(false))
                .count();
            if c > 0 {
                n += c;
                evs.push(ev.clone());
            }
        }
    }
    (n, evs)
}

pub fn doctor() -> anyhow::Result<()> {
    let cfg = Config::load();
    let ok = |b: bool| if b { "ok " } else { "-- " };
    println!("agent-office doctor");
    let cf = paths::config_file();
    println!("{}config       {}{}", ok(cf.exists()), cf.display(), if cf.exists() { "" } else { " (defaults)" });

    let sock = paths::hook_socket();
    let state = if !sock.exists() {
        "absent (office not running)"
    } else if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
        "listening"
    } else {
        "stale (no listener)"
    };
    println!("{}socket       {} [{state}]", ok(state == "listening"), sock.display());

    match find_office_hook() {
        Some(h) => println!("ok office-hook  {}", h.display()),
        None => println!("-- office-hook  not found (cargo install --path crates/office-hook)"),
    }

    let sp = claude_settings_path();
    match std::fs::read_to_string(&sp).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        Some(v) => {
            let (n, evs) = count_ours(&v);
            let missing: Vec<&str> = CLAUDE_EVENTS.iter().copied().filter(|e| !evs.iter().any(|x| x == e)).collect();
            println!(
                "{}claude hooks {} [{n} entries{}{}]",
                ok(missing.is_empty()),
                sp.display(),
                if missing.is_empty() { String::new() } else { format!(", missing: {}", missing.join(",")) },
                if evs.iter().any(|e| e == "PermissionRequest") { ", PermissionRequest on" } else { "" }
            );
        }
        None => println!("-- claude hooks {} unreadable", sp.display()),
    }
    let ag = gemini_plugins_dir().join(AG_PLUGIN).join("hooks.json");
    println!("{}antigravity  {}", ok(ag.exists()), ag.display());

    let route = Path::new(&cfg.agy_route);
    if route.exists() {
        let out = std::process::Command::new(route).args(["--role", "fallback", "--prompt", "x", "--dry-run"]).output();
        match out {
            Ok(o) => {
                let st = crate::sources::agy_router::parse_dry_run(&String::from_utf8_lossy(&o.stdout));
                println!(
                    "{}agy-route    {} [capacity {:?}, slots {}/{:?}, free {:?} MB, pressure {:?}]",
                    ok(st.capacity.is_some()),
                    route.display(),
                    st.capacity,
                    st.busy_slots,
                    st.max_concurrent,
                    st.free_mb,
                    st.pressure
                );
            }
            Err(e) => println!("-- agy-route    {} failed: {e}", route.display()),
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
        assert_eq!(v["hooks"]["PreToolUse"][1]["hooks"][0]["command"], "OFFICE_SOURCE=claude '/opt/x/office-hook'");
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
        write_antigravity_plugin(&d.join(AG_PLUGIN), Path::new("/x/office-hook")).unwrap();
        let h: Value = serde_json::from_str(&std::fs::read_to_string(d.join(AG_PLUGIN).join("hooks.json")).unwrap()).unwrap();
        assert_eq!(h[AG_PLUGIN]["Stop"][0]["command"], "OFFICE_SOURCE=antigravity '/x/office-hook' --hook-event Stop");
        assert_eq!(h[AG_PLUGIN]["PreToolUse"][0]["matcher"], "*");
        let p: Value = serde_json::from_str(&std::fs::read_to_string(d.join(AG_PLUGIN).join("plugin.json")).unwrap()).unwrap();
        assert_eq!(p["name"], AG_PLUGIN);
        let _ = std::fs::remove_dir_all(&d);
    }
}
