//! Native cross-platform pixel-art desktop app for Agent Office (`eframe`/`egui`).

mod ui;

use eframe::egui;
use office::app::chime::{Chime, Chimer, SystemPlayer};
use office::app::state::idle_kill_candidates;
use office::render::Renderer;
use office::sources::hook_socket::SocketGuard;
use office_core::config::Config;
use office_core::model::{AgentId, AgentState, AlertLevel};
use office_core::{paths, Event, EventKind, SourceKind};
use office_core::reducer::Reducer;
use office_core::whiteboard::Whiteboard;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ui::{
    chime_label, configure_visuals, draw_desktop_ui, logical_to_hit_coords, ordered_themes, pixel_texture_options,
    rgba_to_color_image, DesktopState, UiAction, ViewportFrame,
};

const USAGE: &str = "\
office-desktop: native pixel-art desktop office for your AI coding agents

USAGE:
    office-desktop              run the desktop office (live sources)
    office-desktop --demo       run with synthetic agents (no hooks needed)
    office-desktop -h | --help  show this help
    office-desktop -V | --version
                                show the version";

/// Global hook-socket guard so panic hooks, signal exits, `on_exit`, and post-`run_native`
/// cleanup always remove `office.sock` / `office.pid` cleanly (REL-5).
static SOCK_GUARD: Mutex<Option<SocketGuard>> = Mutex::new(None);

fn store_sock_guard(guard: Option<SocketGuard>) {
    if let Ok(mut slot) = SOCK_GUARD.lock() {
        *slot = guard;
    }
}

fn drop_sock_guard() {
    if let Ok(mut slot) = SOCK_GUARD.lock() {
        slot.take();
    }
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        drop_sock_guard();
        prev(info);
    }));
}

struct DesktopApp {
    state: DesktopState,
    reducer: Reducer,
    renderer: Renderer,
    rx: Receiver<Event>,
    tx: Sender<Event>,
    player: SystemPlayer,
    chimer: Chimer,
    texture: Option<egui::TextureHandle>,
    quit_flag: Arc<AtomicBool>,
    fps: u32,
    last_avail_points: (f32, f32),
    last_frame_view: (u32, u32, f32),
    last_sweep_ms: u64,
    last_theme_err_count: usize,
    prev_states: HashMap<AgentId, AgentState>,
    last_inbox_id: u64,
    last_alert_ms: u64,
}

impl DesktopApp {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, demo: bool, quit_flag: Arc<AtomicBool>) -> Self {
        configure_visuals(&cc.egui_ctx);

        let now_ms = paths::now_ms();
        let wb_path = paths::whiteboard_file();
        let (whiteboard, wb_warn) = Whiteboard::load_from(&wb_path);

        let mut renderer = Renderer::new(&cfg);
        renderer.set_mix(&cfg.theme);
        renderer.set_show(&cfg.show);
        renderer.set_ceo(&cfg.ceo);
        renderer.set_whiteboard(&whiteboard);

        let discovered_themes = renderer.themes();
        let discovered_outfits = office_core::assets::list_outfits();

        let mut state = DesktopState::new(
            &cfg,
            demo,
            discovered_themes,
            discovered_outfits,
            whiteboard,
            Some(wb_path),
            Some(paths::config_file()),
        );

        if let Some(warn) = wb_warn {
            state.push_banner(AlertLevel::Warn, format!("Whiteboard warning: {warn}"));
        }

        let last_theme_err_count = surface_theme_errors(&renderer, 0, &mut state);

        let (tx, rx) = mpsc::channel::<Event>();
        if demo {
            office::app::demo::spawn(tx.clone());
        } else {
            let c2 = cfg.clone();
            let t2 = tx.clone();
            match office::app::guarded(move || office::sources::spawn_all(&c2, t2)) {
                Ok(Some(guard)) => {
                    store_sock_guard(Some(guard));
                }
                Ok(None) => {
                    state.push_banner(
                        AlertLevel::Warn,
                        "Hook socket unavailable (another office may own it or bind failed); running without live hook IPC.",
                    );
                }
                Err(e) => {
                    state.push_banner(
                        AlertLevel::Critical,
                        format!("Background sources failed to start ({e}); running in degraded mode."),
                    );
                }
            }
        }

        let player = SystemPlayer::new(cfg.sounds.clone());
        let reducer = Reducer::with_config(&cfg);

        Self {
            state,
            reducer,
            renderer,
            rx,
            tx,
            player,
            chimer: Chimer::default(),
            texture: None,
            quit_flag,
            fps: cfg.fps.clamp(1, 60),
            last_avail_points: (900.0, 660.0),
            last_frame_view: (0, 0, 1.0),
            last_sweep_ms: now_ms,
            last_theme_err_count,
            prev_states: HashMap::new(),
            last_inbox_id: 0,
            last_alert_ms: now_ms,
        }
    }

    fn play_chime_if_allowed(&mut self, chime: Chime, now_ms: u64) {
        if !self.chimer.allow(self.state.sound, chime, now_ms) {
            return;
        }
        if let Err(err) = self.player.play_checked(chime) {
            self.state.push_banner(
                AlertLevel::Warn,
                format!("Sound playback failed ({}): {err}", chime_label(chime)),
            );
            self.state
                .push_toast(AlertLevel::Warn, format!("Sound error: {err}"), now_ms);
        }
    }

    fn observe_scene_events(&mut self, now_ms: u64) {
        let (needs_you_hit, newest_boss, newest_crit) = {
            let scene = self.reducer.scene();

            let mut needs_you_hit: Option<String> = None;
            for (id, a) in &scene.agents {
                let prev = self.prev_states.get(id).copied();
                if a.state == AgentState::NeedsYou && prev != Some(AgentState::NeedsYou) {
                    needs_you_hit = Some(format!("{}: {}", a.label, a.activity));
                }
            }
            self.prev_states = scene.agents.iter().map(|(k, v)| (k.clone(), v.state)).collect();

            let mut newest_boss: Option<(u64, String, String)> = None;
            for r in &scene.reports {
                if r.to.is_none() && r.id > self.last_inbox_id {
                    newest_boss = Some((r.id, r.from_label.clone(), r.title.clone()));
                }
            }

            let mut newest_crit: Option<(u64, String)> = None;
            for a in &scene.alerts {
                if a.level == AlertLevel::Critical && a.at_ms > self.last_alert_ms {
                    newest_crit = Some((a.at_ms, a.message.clone()));
                }
            }

            (needs_you_hit, newest_boss, newest_crit)
        };

        if let Some(body) = needs_you_hit {
            self.play_chime_if_allowed(Chime::NeedsYou, now_ms);
            if self.state.notifications {
                office::sources::control::notify("Agent Office — Needs You", &body);
            }
        }

        if let Some((id, from, title)) = newest_boss {
            self.last_inbox_id = id;
            self.play_chime_if_allowed(Chime::Inbox, now_ms);
            if self.state.notifications {
                office::sources::control::notify(&format!("Boss Inbox — {from}"), &title);
            }
        }

        if let Some((at, msg)) = newest_crit {
            self.last_alert_ms = at;
            self.play_chime_if_allowed(Chime::Error, now_ms);
            if self.state.notifications {
                office::sources::control::notify("Agent Office — Alert", &msg);
            }
        }
    }

    fn execute_action(&mut self, action: UiAction, now_ms: u64) {
        match action {
            UiAction::ApprovePermission { id, request_id } => {
                self.resolve_permission(&id, &request_id, true, now_ms);
            }
            UiAction::DenyPermission { id, request_id } => {
                self.resolve_permission(&id, &request_id, false, now_ms);
            }
            UiAction::CancelAgent { id, pid } => {
                if self.state.demo {
                    let _ = self.tx.send(Event::now(
                        SourceKind::Demo,
                        EventKind::State {
                            id: id.clone(),
                            state: AgentState::Leaving,
                            activity: Some("cancelled by you".into()),
                        },
                    ));
                    let _ = self.tx.send(Event::now(SourceKind::Demo, EventKind::Gone { id: id.clone() }));
                    self.state.push_toast(AlertLevel::Info, format!("(demo) Cancelled {id}"), now_ms);
                } else {
                    let res = office::sources::control::kill_agent_pid(pid)
                        .or_else(|_| office::sources::control::cancel_agy(pid));
                    match res {
                        Ok(()) => self.state.push_toast(AlertLevel::Info, format!("Sent stop signal to pid {pid}"), now_ms),
                        Err(e) => self.state.push_banner(AlertLevel::Warn, format!("Cancel failed for {id}: {e}")),
                    }
                }
            }
            UiAction::ResumeAgent { id } => {
                let Some(agent) = self.reducer.scene().agents.get(&id).cloned() else {
                    return;
                };
                let Some(rid) = agent.resume_id.as_deref() else {
                    return;
                };
                if self.state.demo {
                    let cmd = office::sources::control::resume_command_for(agent.kind, agent.cwd.as_deref(), rid);
                    self.state.push_toast(AlertLevel::Info, format!("(demo) {cmd}"), now_ms);
                } else {
                    match office::sources::control::resume_agent(agent.kind, agent.cwd.as_deref(), rid) {
                        Ok(cmd) => self.state.push_toast(AlertLevel::Info, format!("Resumed: {cmd}"), now_ms),
                        Err(e) => self.state.push_banner(AlertLevel::Warn, format!("Resume failed: {e}")),
                    }
                }
            }
            UiAction::KillIdleAgents => {
                let targets = idle_kill_candidates(self.reducer.scene(), now_ms);
                if targets.is_empty() {
                    return;
                }
                let mut stopped = 0usize;
                for t in targets {
                    if self.state.demo {
                        let _ = self.tx.send(Event::now(
                            SourceKind::Demo,
                            EventKind::State {
                                id: t.id.clone(),
                                state: AgentState::Leaving,
                                activity: Some("stopped by you".into()),
                            },
                        ));
                        let _ = self.tx.send(Event::now(SourceKind::Demo, EventKind::Gone { id: t.id }));
                        stopped += 1;
                    } else if office::sources::control::kill_agent_pid(t.pid).is_ok() {
                        stopped += 1;
                    }
                }
                self.state
                    .push_toast(AlertLevel::Info, format!("Stopped {stopped} idle agent(s)"), now_ms);
            }
            UiAction::ReapOrphans => {
                if self.state.demo {
                    self.state.push_toast(AlertLevel::Info, "(demo) Reaped 0 orphans", now_ms);
                } else {
                    match office::sources::control::reap_orphan_agy() {
                        Ok((n, mb)) => self
                            .state
                            .push_toast(AlertLevel::Info, format!("Reaped {n} orphan(s) (~{mb} MB)"), now_ms),
                        Err(e) => self.state.push_banner(AlertLevel::Warn, format!("Reap orphans failed: {e}")),
                    }
                }
            }
            UiAction::ApplyThemeMix => {
                self.renderer.set_mix(&self.state.mix);
                self.last_theme_err_count =
                    surface_theme_errors(&self.renderer, self.last_theme_err_count, &mut self.state);
            }
            UiAction::ReloadThemes => {
                self.renderer.reload_themes();
                self.state.available_themes = ordered_themes(&self.renderer.themes());
                self.last_theme_err_count = surface_theme_errors(&self.renderer, 0, &mut self.state);
                self.state.push_toast(AlertLevel::Info, "Reloaded themes from disk", now_ms);
            }
            UiAction::ApplyCeo => {
                self.renderer.set_ceo(&self.state.ceo);
                self.last_theme_err_count =
                    surface_theme_errors(&self.renderer, self.last_theme_err_count, &mut self.state);
            }
            UiAction::SyncWhiteboardToRenderer => {
                self.renderer.set_whiteboard(&self.state.whiteboard);
            }
            UiAction::ToggleSound(on) => {
                if on {
                    self.play_chime_if_allowed(Chime::Test, now_ms);
                    self.state.push_toast(AlertLevel::Info, "Sound enabled", now_ms);
                } else {
                    self.state.push_toast(AlertLevel::Info, "Sound muted", now_ms);
                }
            }
            UiAction::ToggleNotifications(on) => {
                self.state.push_toast(
                    AlertLevel::Info,
                    if on {
                        "Desktop notifications enabled"
                    } else {
                        "Desktop notifications muted"
                    },
                    now_ms,
                );
            }
            UiAction::SoundSlotUpdated(sounds) => {
                self.player = SystemPlayer::new(sounds);
            }
            UiAction::RefreshOutfits => {
                self.state.available_outfits = office_core::assets::list_outfits();
            }
        }
    }

    fn resolve_permission(&mut self, id: &str, request_id: &str, allow: bool, now_ms: u64) {
        let verb = if allow { "Approved" } else { "Denied" };
        if self.state.demo {
            let _ = self.tx.send(Event::now(
                SourceKind::Demo,
                EventKind::PermissionResolved {
                    id: id.to_string(),
                    request_id: request_id.to_string(),
                },
            ));
            let (next_state, act) = if allow {
                (AgentState::Running, "approved: running it")
            } else {
                (AgentState::Thinking, "denied: finding another way")
            };
            let _ = self.tx.send(Event::now(
                SourceKind::Demo,
                EventKind::State {
                    id: id.to_string(),
                    state: next_state,
                    activity: Some(act.into()),
                },
            ));
            self.state.push_toast(AlertLevel::Info, format!("(demo) {verb} permission for {id}"), now_ms);
        } else {
            match office::sources::control::resolve_permission(request_id, allow) {
                Ok(()) => self.state.push_toast(AlertLevel::Info, format!("{verb} permission for {id}"), now_ms),
                Err(e) => self.state.push_banner(AlertLevel::Warn, format!("Permission action failed: {e}")),
            }
        }
    }
}

fn surface_theme_errors(renderer: &Renderer, prev_count: usize, state: &mut DesktopState) -> usize {
    let errs = renderer.theme_errors();
    if errs.len() > prev_count {
        for err in &errs[prev_count..] {
            state.push_banner(AlertLevel::Warn, format!("Theme/asset warning: {err}"));
        }
    }
    errs.len()
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.quit_flag.load(Ordering::Relaxed) {
            drop_sock_guard();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let now_ms = paths::now_ms();

        // 1. Drain incoming live/demo events into the shared reducer.
        let mut scene_changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            scene_changed |= self.reducer.apply(ev);
        }

        // 2. Periodic sweep + toast expiration.
        if now_ms.saturating_sub(self.last_sweep_ms) >= 500 {
            self.last_sweep_ms = now_ms;
            scene_changed |= self.reducer.sweep(now_ms);
            self.state.expire_toasts(now_ms);
        }

        if scene_changed {
            self.observe_scene_events(now_ms);
            if self.state.whiteboard.sync_from_scene(self.reducer.scene()) {
                self.renderer.set_whiteboard(&self.state.whiteboard);
            }
        }

        // 3. Compose 1× logical pixel-art frame & upload with TextureOptions::NEAREST.
        let ppp = ctx.pixels_per_point().max(1.0);
        let vp_w = (self.last_avail_points.0 * ppp).round().max(64.0) as u32;
        let vp_h = (self.last_avail_points.1 * ppp).round().max(64.0) as u32;

        let (logical_size, backdrop) = {
            let scene = self.reducer.scene();
            let lf = self
                .renderer
                .compose_logical(scene, self.state.selected_agent.as_ref(), vp_w, vp_h, now_ms);
            self.last_frame_view = (lf.ox, lf.oy, lf.scale);
            let (lw, lh) = (lf.img.width(), lf.img.height());
            let color_img = rgba_to_color_image(lf.img);
            match &mut self.texture {
                Some(tex) => tex.set(color_img, pixel_texture_options()),
                None => {
                    self.texture = Some(ctx.load_texture("office_canvas", color_img, pixel_texture_options()));
                }
            }
            ((lw, lh), lf.backdrop)
        };

        // 4. Draw UI & collect interactions.
        let vf = self.texture.as_ref().map(|tex| ViewportFrame {
            texture: tex,
            logical_size,
            backdrop,
        });
        let (actions, vp_out) = draw_desktop_ui(ctx, &mut self.state, self.reducer.scene(), vf, now_ms);
        if vp_out.avail_points.0 > 1.0 && vp_out.avail_points.1 > 1.0 {
            self.last_avail_points = vp_out.avail_points;
        }

        // 5. Hover & click hit-testing against pixel-art renderer.
        let (ox, oy, scale) = self.last_frame_view;
        let hovered_id = vp_out.hover_logical.and_then(|(lx, ly)| {
            let (fx, fy) = logical_to_hit_coords((lx, ly), ox, oy, scale);
            self.renderer.hit_test(fx, fy)
        });
        if hovered_id != self.state.hovered_id {
            self.state.hovered_id = hovered_id.clone();
            self.renderer.set_hover(hovered_id);
        }

        if let Some((lx, ly)) = vp_out.click_logical {
            let (fx, fy) = logical_to_hit_coords((lx, ly), ox, oy, scale);
            let hit = self.renderer.hit_test(fx, fy);
            self.state.handle_viewport_hit(hit);
        }

        // 6. Execute UI-triggered control/asset/config actions.
        for action in actions {
            self.execute_action(action, now_ms);
        }

        // 7. Request smooth repaint at `cfg.fps`.
        let frame_ms = (1000 / self.fps.max(1) as u64).max(16);
        ctx.request_repaint_after(Duration::from_millis(frame_ms));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        drop_sock_guard();
    }
}

fn parse_args(args: &[String]) -> Result<Option<bool>, String> {
    let mut demo = false;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("office-desktop {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--demo" => {
                demo = true;
            }
            other => {
                return Err(format!("office-desktop: unknown argument {other:?}\n\n{USAGE}"));
            }
        }
    }
    Ok(Some(demo))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo = match parse_args(&args) {
        Ok(Some(d)) => d,
        Ok(None) => return Ok(()),
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    install_panic_hook();

    let quit_flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let _ = signal_hook::flag::register(libc::SIGTERM, Arc::clone(&quit_flag));
        let _ = signal_hook::flag::register(libc::SIGHUP, Arc::clone(&quit_flag));
    }

    let cfg = Config::load();
    let title = if demo {
        "Agent Office (Demo)"
    } else {
        "Agent Office"
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1320.0, 780.0])
            .with_min_inner_size([840.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    let res = eframe::run_native(
        title,
        options,
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, cfg, demo, quit_flag)))),
    );

    // Always clean up socket/pid even if eframe exited abnormally.
    drop_sock_guard();

    res.map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_handles_demo_help_version_and_unknown() {
        assert_eq!(parse_args(&[]).unwrap(), Some(false));
        assert_eq!(parse_args(&["--demo".into()]).unwrap(), Some(true));
        assert_eq!(parse_args(&["-h".into()]).unwrap(), None);
        assert_eq!(parse_args(&["--help".into()]).unwrap(), None);
        assert_eq!(parse_args(&["-V".into()]).unwrap(), None);
        assert_eq!(parse_args(&["--version".into()]).unwrap(), None);
        assert!(parse_args(&["--bogus".into()]).is_err());
    }
}
