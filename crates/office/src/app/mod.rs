//! Workstream C. TUI: event loop, office widget (ratatui-image), sidebar, keys, control actions, notifications.
pub mod demo;
pub mod fmt;
pub mod state;
pub mod ui;

use crate::{render, sources};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use office_core::{config::Config, config::ThemeMix, Agent, AgentId, AlertLevel, Event, EventKind, Reducer, Scene, SourceKind};
use ratatui::{backend::CrosstermBackend, Terminal};
use ratatui_image::picker::{Picker, ProtocolType};
use state::{App, Control, Outcome, ThemeHost};
use std::cell::Cell;
use std::io::Stdout;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

/// What the office widget needs from the renderer (workstream B); faked in tests.
pub trait OfficeView: ThemeHost {
    fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage;
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
}

impl OfficeView for RendererView {
    fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage {
        self.0.compose(scene, selected, px_w, px_h, t_ms)
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

fn restore_terminal() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture, crossterm::cursor::Show);
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

/// Whether the office has something moving (so we redraw at fps even without events).
fn animating(scene: &Scene) -> bool {
    use office_core::AgentState::*;
    scene.agents.values().any(|a| !matches!(a.state, Idle | Done | Queued))
}

pub fn run(cfg: Config, demo: bool) -> anyhow::Result<()> {
    // The capability query toggles raw mode itself, so it runs before our terminal setup.
    let picker = make_picker();
    install_panic_hook();
    let (mut term, _guard) = setup_terminal()?;
    let res = event_loop(&mut term, cfg, demo, picker);
    drop(term);
    restore_terminal();
    res
}

fn event_loop(term: &mut Terminal<CrosstermBackend<Stdout>>, cfg: Config, demo: bool, picker: Picker) -> anyhow::Result<()> {
    let now = office_core::paths::now_ms;
    let mut app = App::new(&cfg, demo);
    let (tx, rx) = mpsc::channel::<Event>();

    let mut ctl: Box<dyn Control> = if demo {
        demo::spawn(tx.clone());
        Box::new(DemoControl { tx: tx.clone() })
    } else {
        let c2 = cfg.clone();
        let t2 = tx.clone();
        if let Err(e) = guarded(move || sources::spawn_all(&c2, t2)) {
            app.toast(format!("sources unavailable: {e}"), AlertLevel::Critical, now());
        }
        Box::new(RealControl)
    };

    let view: Option<Box<dyn OfficeView>> = match guarded(|| render::Renderer::new(&cfg)) {
        Ok(r) => {
            let mut v = RendererView(r);
            v.set_mix(&cfg.theme);
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
    let frame_ms = 1000 / cfg.fps.clamp(1, 60) as u64;
    let mut last_sweep = 0u64;
    let mut last_draw = 0u64;
    let mut reducer_broken = false;
    let mut scene_changed = true;

    loop {
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
        // 3. Draw: on change, at fps while animating, and at least once a second (elapsed times).
        let since = t.saturating_sub(last_draw);
        let anim = animating(reducer.scene());
        if app.dirty || (anim && since >= frame_ms) || since >= 1000 {
            app.dirty = false;
            last_draw = t;
            let scene = reducer.scene();
            term.draw(|f| ui::draw(f, &mut app, scene, &mut office, t))?;
        }
        // 4. Input (bounded wait so the channel stays responsive).
        let wait = if anim { frame_ms.saturating_sub(now().saturating_sub(last_draw)).max(5) } else { 100 };
        if event::poll(Duration::from_millis(wait.min(100)))? {
            let t = now();
            match event::read()? {
                TermEvent::Key(k) => {
                    let scene = reducer.scene();
                    let theme: &mut dyn ThemeHost = match office.view.as_deref_mut() {
                        Some(v) => v,
                        None => &mut NullTheme,
                    };
                    if app.handle_key(k, scene, theme, ctl.as_mut(), t) == Outcome::Quit {
                        break;
                    }
                }
                TermEvent::Mouse(m) => {
                    let view = office.view.as_deref();
                    let hit = |x: u32, y: u32| view.and_then(|v| v.hit_test(x, y));
                    if app.handle_mouse(m, reducer.scene(), &hit) {
                        app.dirty = true;
                    }
                }
                TermEvent::Resize(_, _) => app.dirty = true,
                _ => {}
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
            slot: None,
            pid: None,
            state: AgentState::Idle,
            activity: String::new(),
            started_ms: 0,
            last_event_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
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
        pub composed: Arc<Mutex<Vec<(u32, u32)>>>,
    }
    impl FakeView {
        pub fn new(themes: &[&str]) -> Self {
            FakeView { themes: themes.iter().map(|s| s.to_string()).collect(), mix: ThemeMix::default(), composed: Default::default() }
        }
    }
    impl ThemeHost for FakeView {
        fn themes(&self) -> Vec<String> {
            self.themes.clone()
        }
        fn set_mix(&mut self, mix: &ThemeMix) {
            self.mix = mix.clone();
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
    }
}

#[cfg(test)]
mod tests {
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
}
