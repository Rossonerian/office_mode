//! Workstream C. TUI: event loop, office widget (ratatui-image), sidebar, keys, control actions, notifications.
pub mod chime;
pub mod demo;
pub mod fmt;
pub mod kitty;
pub mod reader;
pub mod state;
pub mod ui;

use crate::{render, sources};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, MouseEvent, MouseEventKind},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use office_core::{config::Config, config::ThemeMix, Agent, AgentId, AlertLevel, Event, EventKind, Reducer, Scene, SourceKind};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};
use ratatui_image::picker::{Picker, ProtocolType};
use state::{App, Control, Outcome, ThemeHost};
use std::cell::Cell;
use std::io::{Stdout, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the office widget needs from the renderer (workstream B); faked in tests.
pub trait OfficeView: ThemeHost {
    fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage;
    fn compose_logical(
        &mut self,
        _scene: &Scene,
        _selected: Option<&AgentId>,
        _px_w: u32,
        _px_h: u32,
        _t_ms: u64,
    ) -> Option<render::LogicalFrame<'_>> {
        None
    }
    fn hit_test(&self, x: u32, y: u32) -> Option<AgentId>;
    /// What the mouse is over (the renderer shows the cat's name on hover).
    fn set_hover(&mut self, _id: Option<AgentId>) {}
}

/// render::Renderer behind panic guards, so an unfinished renderer degrades to text mode.
struct RendererView(render::Renderer);

impl ThemeHost for RendererView {
    fn themes(&self) -> Vec<String> {
        let mut t = guarded(|| self.0.themes()).unwrap_or_default();
        if !t.iter().any(|n| n == "procedural") {
            t.insert(0, "procedural".into());
        }
        t
    }
    fn set_mix(&mut self, mix: &ThemeMix) {
        let _ = guarded(|| self.0.set_mix(mix));
    }
    fn set_show(&mut self, show: &office_core::config::ShowConfig) {
        let _ = guarded(|| self.0.set_show(show));
    }
}

impl OfficeView for RendererView {
    fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage {
        self.0.compose(scene, selected, px_w, px_h, t_ms)
    }
    fn compose_logical(
        &mut self,
        scene: &Scene,
        selected: Option<&AgentId>,
        px_w: u32,
        px_h: u32,
        t_ms: u64,
    ) -> Option<render::LogicalFrame<'_>> {
        Some(self.0.compose_logical(scene, selected, px_w, px_h, t_ms))
    }
    fn hit_test(&self, x: u32, y: u32) -> Option<AgentId> {
        guarded(|| self.0.hit_test(x, y)).ok().flatten()
    }
    fn set_hover(&mut self, id: Option<AgentId>) {
        self.0.set_hover(id);
    }
}

struct NullTheme;
impl ThemeHost for NullTheme {
    fn themes(&self) -> Vec<String> {
        vec!["procedural".into()]
    }
    fn set_mix(&mut self, _mix: &ThemeMix) {}
}

// ---------- panic safety ----------

thread_local! {
    static QUIET_PANIC: Cell<bool> = const { Cell::new(false) };
}
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);
static BG_PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// Run `f`, turning a panic (e.g. a `todo!()` in another workstream's code) into an Err
/// without tearing down the terminal.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let prev = QUIET_PANIC.with(|q| q.replace(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    QUIET_PANIC.with(|q| q.set(prev));
    r.map_err(|e| panic_text(&*e))
}

/// Set once the controlling terminal is known to be gone (orphaned / hung up): restore writes are skipped.
static TERMINAL_GONE: AtomicBool = AtomicBool::new(false);
/// The hook-socket guard lives here so the watchdog can remove office.sock / office.pid before a forced exit.
static SOCK_GUARD: Mutex<Option<sources::hook_socket::SocketGuard>> = Mutex::new(None);

fn drop_sock_guard() {
    if let Ok(mut g) = SOCK_GUARD.lock() {
        g.take();
    }
}

/// True when we were started under a real parent and have since been re-parented to launchd/init.
pub fn is_orphaned(start_ppid: i32, cur_ppid: i32) -> bool {
    start_ppid != 1 && cur_ppid == 1
}

/// What the quit watchdog should do on a tick. `since_trigger` is how long ago quitting began.
#[derive(Debug, PartialEq, Eq)]
pub enum WatchdogAction {
    Wait,
    Exit,
}

pub const WATCHDOG_GRACE: Duration = Duration::from_millis(1500);
/// With the terminal gone nothing can be restored, so only a short grace for a clean exit.
pub const WATCHDOG_GRACE_ORPHAN: Duration = Duration::from_millis(200);

pub fn watchdog_action(since_trigger: Option<Duration>, orphan: bool) -> WatchdogAction {
    let grace = if orphan { WATCHDOG_GRACE_ORPHAN } else { WATCHDOG_GRACE };
    match since_trigger {
        Some(d) if d >= grace => WatchdogAction::Exit,
        _ => WatchdogAction::Wait,
    }
}

/// Independent of the event loop (which can be wedged inside crossterm's read on a hung-up tty):
/// once quitting starts, force the process out if it hasn't exited shortly after.
fn spawn_quit_watchdog(quit_flag: Arc<AtomicBool>, start_ppid: i32) {
    let _ = std::thread::Builder::new().name("quit-watchdog".into()).spawn(move || {
        let mut trigger: Option<std::time::Instant> = None;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            #[cfg(unix)]
            let orphan = is_orphaned(start_ppid, unsafe { libc::getppid() });
            #[cfg(not(unix))]
            let orphan = false;
            if orphan {
                TERMINAL_GONE.store(true, Ordering::SeqCst);
            }
            if trigger.is_none() && (orphan || quit_flag.load(Ordering::Relaxed)) {
                trigger = Some(std::time::Instant::now());
            }
            if watchdog_action(trigger.map(|t| t.elapsed()), TERMINAL_GONE.load(Ordering::SeqCst)) == WatchdogAction::Exit {
                drop_sock_guard();
                std::process::exit(0);
            }
        }
    });
}

fn restore_terminal() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        if TERMINAL_GONE.load(Ordering::SeqCst) {
            return;
        }
        let is_tmux = std::env::var_os("TMUX").is_some();
        let del = kitty::delete_seqs(is_tmux);
        let mut so = std::io::stdout();
        // A write error (EIO on a hung-up pty) means the terminal is gone: don't try anything else.
        if so.write_all(&del).is_err() {
            TERMINAL_GONE.store(true, Ordering::SeqCst);
            return;
        }
        // Harmless if no synchronized update is open; unfreezes the screen if one was left open.
        let _ = execute!(so, EndSynchronizedUpdate);
        if so.flush().is_err() {
            TERMINAL_GONE.store(true, Ordering::SeqCst);
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(so, LeaveAlternateScreen, DisableMouseCapture, crossterm::cursor::Show);
    }
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if QUIET_PANIC.with(|q| q.get()) {
            return;
        }
        let main = std::thread::current().name() == Some("main");
        if !main && TERMINAL_ACTIVE.load(Ordering::SeqCst) {
            // A background source died: report it in the UI instead of scribbling over it.
            if let Ok(mut v) = BG_PANICS.lock() {
                v.push(info.to_string());
            }
            return;
        }
        restore_terminal();
        default(info);
    }));
}

struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn setup_terminal() -> anyhow::Result<(Terminal<CrosstermBackend<Stdout>>, TermGuard)> {
    enable_raw_mode()?;
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
    let guard = TermGuard;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut t = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    t.clear()?;
    Ok((t, guard))
}

// ---------- control backends ----------

/// Real control actions (workstream A's sources::control), panic-guarded.
struct RealControl;

fn not_ready(e: String) -> anyhow::Error {
    anyhow::anyhow!("control action unavailable ({e})")
}

impl Control for RealControl {
    fn cancel_agy(&mut self, _agent: &Agent, pid: u32) -> anyhow::Result<()> {
        guarded(|| sources::control::cancel_agy(pid)).map_err(not_ready)?
    }
    fn resolve_permission(&mut self, _agent: &Agent, request_id: &str, allow: bool) -> anyhow::Result<()> {
        guarded(|| sources::control::resolve_permission(request_id, allow)).map_err(not_ready)?
    }
    fn resume_claude(&mut self, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
        guarded(|| sources::control::resume_claude(cwd, session_id)).map_err(not_ready)?
    }
    fn notify(&mut self, title: &str, body: &str) {
        let _ = guarded(|| sources::control::notify(title, body));
    }
    fn play_sound(&mut self, chime: chime::Chime) {
        use chime::SoundPlayer;
        let _ = guarded(|| chime::AfPlayer.play(chime));
    }
}

/// Demo control: never touches real processes; answers with synthetic events.
pub struct DemoControl {
    pub tx: Sender<Event>,
}

impl Control for DemoControl {
    fn cancel_agy(&mut self, agent: &Agent, _pid: u32) -> anyhow::Result<()> {
        let id = agent.id.clone();
        let _ = self.tx.send(Event::now(
            SourceKind::Demo,
            EventKind::State { id: id.clone(), state: office_core::AgentState::Leaving, activity: Some("cancelled by you".into()) },
        ));
        let _ = self.tx.send(Event::now(SourceKind::Demo, EventKind::Gone { id }));
        Ok(())
    }
    fn resolve_permission(&mut self, agent: &Agent, request_id: &str, allow: bool) -> anyhow::Result<()> {
        let id = agent.id.clone();
        let _ = self.tx.send(Event::now(SourceKind::Demo, EventKind::PermissionResolved { id: id.clone(), request_id: request_id.into() }));
        let (state, act) = if allow {
            (office_core::AgentState::Running, "approved: running it")
        } else {
            (office_core::AgentState::Thinking, "denied: finding another way")
        };
        let _ = self.tx.send(Event::now(SourceKind::Demo, EventKind::State { id, state, activity: Some(act.into()) }));
        Ok(())
    }
    fn resume_claude(&mut self, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
        Ok(match cwd {
            Some(c) => format!("(demo) cd {c} && claude --resume {session_id}"),
            None => format!("(demo) claude --resume {session_id}"),
        })
    }
    fn notify(&mut self, title: &str, body: &str) {
        let _ = guarded(|| sources::control::notify(title, body));
    }
    fn play_sound(&mut self, chime: chime::Chime) {
        use chime::SoundPlayer;
        let _ = guarded(|| chime::AfPlayer.play(chime));
    }
}

// ---------- the loop ----------

fn make_picker() -> Picker {
    match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(_) => {
            let mut p = Picker::from_fontsize((10, 20));
            p.set_protocol_type(ProtocolType::Halfblocks);
            p
        }
    }
}

/// Target redraw cadence in ms: `fps` when active or walking, 6 FPS for smooth ambient cat/CEO/queued motion,
/// or 1000 ms for elapsed counters only.
pub fn frame_interval_ms(scene: &Scene, cfg: &Config, now_ms: u64, walking: bool) -> u64 {
    use office_core::AgentState::*;
    let active = scene.agents.values().any(|a| {
        !matches!(a.state, Idle | Done | Queued)
            || (a.state == Done && now_ms.saturating_sub(a.last_event_ms) < 3_000)
    });
    if active || walking {
        (1000 / cfg.fps.clamp(1, 60) as u64).max(16)
    } else if cfg.cat.enabled || cfg.ceo.enabled || scene.agents.values().any(|a| a.state == Queued) {
        166
    } else {
        1000
    }
}

pub fn poll_wait_ms(cadence_ms: u64, since_draw_ms: u64) -> u64 {
    cadence_ms.saturating_sub(since_draw_ms).clamp(5, 20)
}

pub fn run(cfg: Config, demo: bool) -> anyhow::Result<()> {
    // The capability query toggles raw mode itself, so it runs before our terminal setup.
    let picker = make_picker();
    install_panic_hook();
    let quit_flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        let _ = signal_hook::flag::register(libc::SIGTERM, Arc::clone(&quit_flag));
        let _ = signal_hook::flag::register(libc::SIGHUP, Arc::clone(&quit_flag));
    }
    let (mut term, _guard) = setup_terminal()?;
    #[cfg(unix)]
    let start_ppid = unsafe { libc::getppid() };
    #[cfg(not(unix))]
    let start_ppid = 0;
    spawn_quit_watchdog(Arc::clone(&quit_flag), start_ppid);
    let res = event_loop(&mut term, cfg, demo, picker, Arc::clone(&quit_flag), start_ppid);
    // The loop is over (quit key, signal, orphan or error): arm the watchdog in case teardown hangs.
    quit_flag.store(true, Ordering::Relaxed);
    drop(term);
    restore_terminal();
    drop_sock_guard();
    res
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    cfg: Config,
    demo: bool,
    picker: Picker,
    quit_flag: Arc<AtomicBool>,
    start_ppid: i32,
) -> anyhow::Result<()> {
    let now = office_core::paths::now_ms;
    let mut app = App::new(&cfg, demo);
    let (tx, rx) = mpsc::channel::<Event>();

    let mut ctl: Box<dyn Control> = if demo {
        demo::spawn(tx.clone());
        Box::new(DemoControl { tx: tx.clone() })
    } else {
        let c2 = cfg.clone();
        let t2 = tx.clone();
        match guarded(move || sources::spawn_all(&c2, t2)) {
            Ok(g) => {
                if let Ok(mut slot) = SOCK_GUARD.lock() {
                    *slot = g;
                }
            }
            Err(e) => app.toast(format!("sources unavailable: {e}"), AlertLevel::Critical, now()),
        }
        Box::new(RealControl)
    };

    let view: Option<Box<dyn OfficeView>> = match guarded(|| render::Renderer::new(&cfg)) {
        Ok(r) => {
            let mut v = RendererView(r);
            v.set_mix(&cfg.theme);
            v.set_show(&cfg.show);
            Some(Box::new(v))
        }
        Err(e) => {
            app.toast(format!("renderer unavailable, text mode ({e})"), AlertLevel::Warn, now());
            None
        }
    };
    let mut office = ui::OfficeCtx::new(view, Some(picker));
    if office.view.is_none() {
        office.error = Some("renderer unavailable".into());
    }

    let mut reducer = Reducer::with_config(&cfg);
    let mut last_sweep = 0u64;
    let mut last_draw = 0u64;
    let mut reducer_broken = false;
    let mut scene_changed = true;

    while !quit_flag.load(Ordering::Relaxed) {
        // The terminal that ran us is gone (we were re-parented to launchd): exit instead of
        // spinning in the background and holding the hook socket.
        #[cfg(unix)]
        if is_orphaned(start_ppid, unsafe { libc::getppid() }) {
            TERMINAL_GONE.store(true, Ordering::SeqCst);
            break;
        }
        let t = now();
        // 1. Events from sources / demo.
        while let Ok(ev) = rx.try_recv() {
            match guarded(|| reducer.apply(ev)) {
                Ok(ch) => scene_changed |= ch,
                Err(e) if !reducer_broken => {
                    reducer_broken = true;
                    app.toast(format!("reducer error: {e}"), AlertLevel::Critical, t);
                }
                Err(_) => {}
            }
        }
        // 2. Housekeeping.
        if t.saturating_sub(last_sweep) >= 500 {
            last_sweep = t;
            if let Ok(ch) = guarded(|| reducer.sweep(t)) {
                scene_changed |= ch;
            }
            if app.expire_toasts(t) {
                app.dirty = true;
            }
            let bg: Vec<String> = BG_PANICS.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
            for p in bg {
                app.toast(format!("source crashed: {p}"), AlertLevel::Critical, t);
            }
        }
        if scene_changed {
            app.observe(reducer.scene(), t, ctl.as_mut());
            scene_changed = false;
        }
        // 3. Draw with Ghostty synchronized output (DEC 2026) and zero-redundant Kitty uploads.
        let since = t.saturating_sub(last_draw);
        let cadence = frame_interval_ms(reducer.scene(), &cfg, t, office.last_walking);
        if app.dirty || since >= cadence {
            app.dirty = false;
            let scene = reducer.scene();
            let sz = term.size()?;
            let office_area = ui::office_rect(Rect::new(0, 0, sz.width, sz.height), app.sidebar);
            office.prepare_frame(&app, scene, office_area, t);
            let pending = office.take_pending_kitty();

            let sync = matches!(office.picker.as_ref().map(|p| p.protocol_type()), Some(ProtocolType::Kitty));
            if sync || pending.is_some() {
                let backend = term.backend_mut();
                if sync {
                    let _ = execute!(backend, BeginSynchronizedUpdate);
                }
                if let Some(bytes) = pending {
                    let _ = backend.write_all(&bytes);
                }
            }
            let drawn = term.draw(|f| ui::draw(f, &mut app, scene, &mut office, t)).map(|_| ());
            if sync {
                // Always close the sync block, even when the draw failed, or the terminal stays frozen.
                let _ = execute!(term.backend_mut(), EndSynchronizedUpdate);
            }
            drawn?;
            last_draw = now();
        }
        // 4. Input: bounded wait, then drain all queued events & coalesce mouse moves.
        let wait = poll_wait_ms(cadence, now().saturating_sub(last_draw));
        if event::poll(Duration::from_millis(wait))? {
            let mut last_move: Option<MouseEvent> = None;
            let mut quit = false;
            for _ in 0..128 {
                let t = now();
                match event::read()? {
                    TermEvent::Key(k) => {
                        let scene = reducer.scene();
                        let theme: &mut dyn ThemeHost = match office.view.as_deref_mut() {
                            Some(v) => v,
                            None => &mut NullTheme,
                        };
                        if app.handle_key(k, scene, theme, ctl.as_mut(), t) == Outcome::Quit {
                            quit = true;
                            break;
                        }
                    }
                    TermEvent::Mouse(m) if m.kind == MouseEventKind::Moved => {
                        last_move = Some(m);
                    }
                    TermEvent::Mouse(m) => {
                        let view = office.view.as_deref();
                        let hit = |x: u32, y: u32| view.and_then(|v| v.hit_test(x, y));
                        if app.handle_mouse_at(m, reducer.scene(), &hit, t) {
                            app.dirty = true;
                        }
                        if let Some(mix) = app.pending_mix.take() {
                            if let Some(v) = office.view.as_deref_mut() {
                                v.set_mix(&mix);
                            }
                        }
                    }
                    TermEvent::Resize(_, _) => app.dirty = true,
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            if quit {
                break;
            }
            if let Some(m) = last_move {
                let t = now();
                let view = office.view.as_deref();
                let hit = |x: u32, y: u32| view.and_then(|v| v.hit_test(x, y));
                if app.handle_mouse_at(m, reducer.scene(), &hit, t) {
                    app.dirty = true;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    use office_core::{model::AgentKind, Alert, AgentState, RouterStatus};
    use std::sync::{Arc, Mutex};

    pub const ALL_STATES: [AgentState; 12] = [
        AgentState::Arriving,
        AgentState::Idle,
        AgentState::Thinking,
        AgentState::Typing,
        AgentState::Reading,
        AgentState::Running,
        AgentState::Delegating,
        AgentState::NeedsYou,
        AgentState::Queued,
        AgentState::Done,
        AgentState::Error,
        AgentState::Leaving,
    ];

    pub fn agent(id: &str, kind: AgentKind, parent: Option<&str>) -> Agent {
        Agent {
            id: id.into(),
            kind,
            label: id.into(),
            parent: parent.map(Into::into),
            cwd: None,
            role: None,
            model: None,
            current_tool: None,
            last_tool: None,
            last_tool_ok: None,
            slot: None,
            pid: None,
            state: AgentState::Idle,
            activity: String::new(),
            started_ms: 0,
            last_event_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            cost_usd: 0.0,
            context_pct: None,
            rss_mb: None,
            tool_calls: 0,
            tool_failures: 0,
            resume_id: None,
            pending_permission: None,
        }
    }

    pub fn scene_with(agents: Vec<Agent>) -> Scene {
        let mut s = Scene::default();
        for a in agents {
            s.agents.insert(a.id.clone(), a);
        }
        s
    }

    /// A fixed scene: two Claude sessions, one subagent, one agy call, router + alerts.
    pub fn demo_scene() -> Scene {
        let mut dice = agent("cc:dice", AgentKind::Claude, None);
        dice.label = "cc·DICE".into();
        dice.cwd = Some("/tmp/dice".into());
        dice.resume_id = Some("sess-dice".into());
        dice.model = Some("claude-opus-4-5".into());
        dice.state = AgentState::Reading;
        dice.activity = "Read src/main.rs".into();
        dice.tokens_in = 9_000;
        dice.tokens_out = 1_000;
        dice.cost_usd = 1.0;
        let mut sub = agent("cc-sub:dice/1", AgentKind::ClaudeSub, Some("cc:dice"));
        sub.label = "Explore".into();
        sub.role = Some("Explore".into());
        sub.started_ms = 10_000;
        sub.state = AgentState::Thinking;
        let mut office = agent("cc:office", AgentKind::Claude, None);
        office.label = "cc·office".into();
        office.cwd = Some("/tmp/office".into());
        office.resume_id = Some("sess-office".into());
        office.started_ms = 100;
        office.state = AgentState::NeedsYou;
        office.activity = "Bash rm -rf build".into();
        office.pending_permission = Some("perm-1".into());
        let mut agy = agent("agy:4242", AgentKind::Agy, Some("cc:office"));
        agy.label = "agy-worker".into();
        agy.role = Some("worker".into());
        agy.model = Some("gemini-4-hydrogen-low".into());
        agy.pid = Some(4242);
        agy.slot = Some(1);
        agy.started_ms = 50_000;
        agy.state = AgentState::Typing;
        agy.tokens_in = 5_100;
        agy.cost_usd = 0.23;
        let mut s = scene_with(vec![dice, sub, office, agy]);
        s.total_tokens = 15_100;
        s.total_cost_usd = 1.23;
        s.router = RouterStatus {
            capacity: Some(3),
            max_concurrent: Some(5),
            busy_slots: 2,
            free_mb: Some(1800),
            per_proc_mb: Some(280),
            pressure: Some(2),
            queued: 1,
            limit_text: Some("capacity 3 reached; 1 call queued behind the worker".into()),
        };
        s.alerts = vec![
            Alert { at_ms: 20_000, level: AlertLevel::Info, agent: None, code: "router".into(), message: "status ok".into() },
            Alert {
                at_ms: 60_000,
                level: AlertLevel::Warn,
                agent: Some("cc-sub:dice/1".into()),
                code: "loop".into(),
                message: "repeated Grep TODO 4x".into(),
            },
        ];
        s.now_ms = 65_000;
        s
    }

    pub fn select(app: &mut state::App, id: &str) {
        app.selected = Some(id.into());
    }

    pub struct FakeView {
        pub themes: Vec<String>,
        pub mix: ThemeMix,
        pub show: office_core::config::ShowConfig,
        pub composed: Arc<Mutex<Vec<(u32, u32)>>>,
    }
    impl FakeView {
        pub fn new(themes: &[&str]) -> Self {
            FakeView { themes: themes.iter().map(|s| s.to_string()).collect(), mix: ThemeMix::default(), show: Default::default(), composed: Default::default() }
        }
    }
    impl ThemeHost for FakeView {
        fn themes(&self) -> Vec<String> {
            self.themes.clone()
        }
        fn set_mix(&mut self, mix: &ThemeMix) {
            self.mix = mix.clone();
        }
        fn set_show(&mut self, show: &office_core::config::ShowConfig) {
            self.show = show.clone();
        }
    }
    impl OfficeView for FakeView {
        fn compose(&mut self, _s: &Scene, _sel: Option<&AgentId>, w: u32, h: u32, _t: u64) -> image::RgbaImage {
            self.composed.lock().unwrap().push((w, h));
            image::RgbaImage::from_pixel(w, h, image::Rgba([40, 60, 80, 255]))
        }
        fn hit_test(&self, _x: u32, _y: u32) -> Option<AgentId> {
            None
        }
    }

    #[derive(Default)]
    pub struct FakeControl {
        pub calls: Vec<String>,
        pub notes: Vec<(String, String)>,
        pub sounds: Vec<chime::Chime>,
        pub fail: bool,
    }
    impl Control for FakeControl {
        fn cancel_agy(&mut self, _a: &Agent, pid: u32) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("boom")
            }
            self.calls.push(format!("cancel {pid}"));
            Ok(())
        }
        fn resolve_permission(&mut self, _a: &Agent, req: &str, allow: bool) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("boom")
            }
            self.calls.push(format!("perm {req} {}", if allow { "allow" } else { "deny" }));
            Ok(())
        }
        fn resume_claude(&mut self, cwd: Option<&str>, id: &str) -> anyhow::Result<String> {
            if self.fail {
                anyhow::bail!("boom")
            }
            self.calls.push(format!("resume {} {id}", cwd.unwrap_or("-")));
            Ok(format!("cd {} && claude --resume {id}", cwd.unwrap_or(".")))
        }
        fn notify(&mut self, title: &str, body: &str) {
            self.notes.push((title.into(), body.into()));
        }
        fn play_sound(&mut self, chime: chime::Chime) {
            self.sounds.push(chime);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn orphan_detection() {
        assert!(super::is_orphaned(500, 1));
        assert!(!super::is_orphaned(500, 500));
        assert!(!super::is_orphaned(1, 1));
    }

    #[test]
    fn watchdog_decisions() {
        use super::{watchdog_action as w, WatchdogAction::*};
        use std::time::Duration as D;
        assert_eq!(w(None, false), Wait);
        assert_eq!(w(Some(D::from_millis(1000)), false), Wait);
        assert_eq!(w(Some(D::from_millis(1500)), false), Exit);
        assert_eq!(w(Some(D::from_millis(250)), true), Exit);
        assert_eq!(w(Some(D::from_millis(100)), true), Wait);
    }

    use super::*;

    #[test]
    fn guarded_catches_todo() {
        let r: Result<u32, String> = guarded(|| todo!("workstream X"));
        assert!(r.unwrap_err().contains("workstream X"));
        assert_eq!(guarded(|| 7).unwrap(), 7);
    }

    #[test]
    fn demo_control_emits_events_not_signals() {
        let (tx, rx) = mpsc::channel();
        let mut c = DemoControl { tx };
        let a = testutil::demo_scene().agents["agy:4242"].clone();
        c.cancel_agy(&a, 4242).unwrap();
        let evs: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(evs.last().unwrap().kind, EventKind::Gone { ref id } if id == "agy:4242"));
        assert!(c.resume_claude(Some("/x"), "s").unwrap().contains("cd /x && claude --resume s"));
    }

    #[test]
    fn poll_wait_ms_and_frame_interval_tiers() {
        assert_eq!(poll_wait_ms(1000, 0), 20);
        assert_eq!(poll_wait_ms(16, 15), 5);
        assert_eq!(poll_wait_ms(20, 10), 10);

        let mut cfg = Config::default();
        cfg.fps = 30;
        cfg.cat.enabled = false;
        cfg.ceo.enabled = false;
        let mut s = Scene::default();
        assert_eq!(frame_interval_ms(&s, &cfg, 0, false), 1000);
        assert_eq!(frame_interval_ms(&s, &cfg, 0, true), 33);
        cfg.cat.enabled = true;
        assert_eq!(frame_interval_ms(&s, &cfg, 0, false), 166);

        // Done: fast inside the 3 s grace window, back to idle cadence after it.
        cfg.cat.enabled = false;
        let mut agent = testutil::demo_scene().agents["agy:4242"].clone();
        agent.state = office_core::AgentState::Done;
        agent.last_event_ms = 10_000;
        s.agents.insert("d".into(), agent);
        assert_eq!(frame_interval_ms(&s, &cfg, 12_999, false), 33);
        assert_eq!(frame_interval_ms(&s, &cfg, 13_000, false), 1000);
        // Queued (not active) uses the slow animation tier even with cat/ceo off.
        s.agents.get_mut("d").unwrap().state = office_core::AgentState::Queued;
        assert_eq!(frame_interval_ms(&s, &cfg, 99_000, false), 166);
        assert_eq!(frame_interval_ms(&s, &cfg, 99_000, true), 33);
        // A working agent drives the full rate.
        s.agents.get_mut("d").unwrap().state = office_core::AgentState::Typing;
        assert_eq!(frame_interval_ms(&s, &cfg, 99_000, false), 33);
    }
}
