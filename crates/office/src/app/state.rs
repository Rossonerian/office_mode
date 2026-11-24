//! App state machine: selection, popups, toasts, key/mouse handling, notifications, theme cycling.
//! Everything here is pure over (Scene, ThemeHost, Control) so it can be tested without a tty.
use super::fmt;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use office_core::{config::ThemeMix, model::AgentKind, Agent, AgentId, AgentState, AlertLevel, Scene};
use ratatui::layout::Rect;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

/// Theme access needed by the key handler (implemented by render::Renderer and test fakes).
pub trait ThemeHost {
    fn themes(&self) -> Vec<String>;
    fn set_mix(&mut self, mix: &ThemeMix);
}

/// Side-effecting control actions (real: sources::control; demo: synthetic events; tests: fakes).
pub trait Control {
    fn cancel_agy(&mut self, agent: &Agent, pid: u32) -> anyhow::Result<()>;
    fn resolve_permission(&mut self, agent: &Agent, request_id: &str, allow: bool) -> anyhow::Result<()>;
    fn resume_claude(&mut self, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String>;
    fn notify(&mut self, title: &str, body: &str);
}

pub const PRESET_ORDER: [&str; 5] = ["procedural", "modern", "cozy", "retro", "cyberpunk"];
pub const LAYERS: [&str; 5] = ["floor", "walls", "furniture", "decor", "characters"];
pub const NOTIFY_INTERVAL_MS: u64 = 10_000;
pub const TOAST_MS: u64 = 4_000;
const HISTORY_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    None,
    Help,
    Detail(AgentId),
    ConfirmCancel { id: AgentId, label: String, pid: u32 },
    Mixer { layer: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub level: AlertLevel,
    pub until_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
}

/// Screen geometry from the last draw (for mouse hit-testing).
#[derive(Debug, Clone, Default)]
pub struct LayoutCache {
    pub office: Rect,
    /// Area of the agent table body (rows only, no header).
    pub agent_rows: Rect,
    /// Agent ids of the visible table rows, top to bottom.
    pub row_ids: Vec<AgentId>,
    /// Terminal font size in pixels (w, h) when an image protocol is active.
    pub font_px: Option<(u16, u16)>,
}

pub fn mix_eq(a: &ThemeMix, b: &ThemeMix) -> bool {
    a.floor == b.floor && a.walls == b.walls && a.furniture == b.furniture && a.decor == b.decor && a.characters == b.characters
}

pub fn uniform_mix(name: &str) -> ThemeMix {
    let n = name.to_string();
    ThemeMix { floor: n.clone(), walls: n.clone(), furniture: n.clone(), decor: n.clone(), characters: n }
}

pub fn layer_get(m: &ThemeMix, layer: usize) -> &str {
    match layer {
        0 => &m.floor,
        1 => &m.walls,
        2 => &m.furniture,
        3 => &m.decor,
        _ => &m.characters,
    }
}

pub fn layer_set(m: &mut ThemeMix, layer: usize, v: String) {
    match layer {
        0 => m.floor = v,
        1 => m.walls = v,
        2 => m.furniture = v,
        3 => m.decor = v,
        _ => m.characters = v,
    }
}

/// Write `theme = {..}` into the TOML config, keeping every other key the user has.
pub fn persist_theme(path: &std::path::Path, mix: &ThemeMix) -> anyhow::Result<()> {
    let mut table: toml::Table = match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml::Table>().unwrap_or_default(),
        Err(_) => toml::Table::new(),
    };
    table.insert("theme".into(), toml::Value::try_from(mix)?);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string_pretty(&table)?)?;
    Ok(())
}

/// Ambient office characters (the cat, the CEO) are selectable but are not agents.
pub fn is_ambient(id: &str) -> bool {
    id.starts_with("office:")
}

pub struct App {
    pub selected: Option<AgentId>,
    /// What the mouse hovers in the office (agent id or an ambient "office:*" id).
    pub hover: Option<AgentId>,
    pub popup: Popup,
    pub sidebar: bool,
    pub notifications: bool,
    pub toasts: Vec<Toast>,
    /// Alerts at or before this time are hidden (the `c` key).
    pub alerts_cleared_at: Option<u64>,
    pub permissions_enabled: bool,
    pub demo: bool,
    /// Where `t`/`T` persist the theme mix; None = don't persist (tests).
    pub config_path: Option<PathBuf>,
    pub mix: ThemeMix,
    pub presets: BTreeMap<String, ThemeMix>,
    /// Recent activity per agent (time, text), newest last.
    pub history: HashMap<AgentId, VecDeque<(u64, String)>>,
    prev_states: HashMap<AgentId, AgentState>,
    seen_alerts: HashSet<(u64, String, Option<AgentId>, String)>,
    last_notify: HashMap<String, u64>,
    pub layout: LayoutCache,
    /// Set whenever the UI needs a redraw independent of scene changes.
    pub dirty: bool,
}

impl App {
    pub fn new(cfg: &office_core::config::Config, demo: bool) -> Self {
        App {
            selected: None,
            hover: None,
            popup: Popup::None,
            sidebar: cfg.sidebar,
            notifications: cfg.notifications,
            toasts: Vec::new(),
            alerts_cleared_at: None,
            permissions_enabled: demo || cfg.permission_timeout_s > 0,
            demo,
            config_path: Some(office_core::paths::config_file()),
            mix: cfg.theme.clone(),
            presets: cfg.presets.clone(),
            history: HashMap::new(),
            prev_states: HashMap::new(),
            seen_alerts: HashSet::new(),
            last_notify: HashMap::new(),
            layout: LayoutCache::default(),
            dirty: true,
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, level: AlertLevel, now_ms: u64) {
        self.toasts.push(Toast { text: text.into(), level, until_ms: now_ms + TOAST_MS });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        self.dirty = true;
    }

    pub fn current_toast(&self) -> Option<&Toast> {
        self.toasts.last()
    }

    /// Drop expired toasts. Returns true if any were removed.
    pub fn expire_toasts(&mut self, now_ms: u64) -> bool {
        let n = self.toasts.len();
        self.toasts.retain(|t| t.until_ms > now_ms);
        n != self.toasts.len()
    }

    pub fn visible_alerts<'a>(&self, scene: &'a Scene) -> Vec<&'a office_core::Alert> {
        let mut v: Vec<_> = scene
            .alerts
            .iter()
            .filter(|a| self.alerts_cleared_at.map_or(true, |c| a.at_ms > c))
            .collect();
        v.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
        v
    }

    pub fn selected_agent<'a>(&self, scene: &'a Scene) -> Option<&'a Agent> {
        self.selected.as_ref().and_then(|id| scene.agents.get(id))
    }

    /// Fold a (possibly changed) scene into UI-side state: selection repair, activity history,
    /// NeedsYou / Critical notifications.
    pub fn observe(&mut self, scene: &Scene, now_ms: u64, ctl: &mut dyn Control) {
        // Selection repair.
        let rows = fmt::agent_rows(scene);
        let sel_ok = self.selected.as_ref().is_some_and(|id| scene.agents.contains_key(id) || is_ambient(id));
        if !sel_ok {
            self.selected = rows.first().map(|(id, _)| id.clone());
        }
        if let Popup::Detail(id) | Popup::ConfirmCancel { id, .. } = &self.popup {
            if !scene.agents.contains_key(id) {
                self.popup = Popup::None;
            }
        }

        // Activity history + state transitions.
        let mut entered_needs_you: Vec<&Agent> = Vec::new();
        for a in scene.agents.values() {
            if !a.activity.is_empty() {
                let h = self.history.entry(a.id.clone()).or_default();
                if h.back().map(|(_, t)| t != &a.activity).unwrap_or(true) {
                    h.push_back((a.last_event_ms.max(1).min(now_ms.max(1)), a.activity.clone()));
                    while h.len() > HISTORY_LEN {
                        h.pop_front();
                    }
                }
            }
            let prev = self.prev_states.insert(a.id.clone(), a.state);
            if a.state == AgentState::NeedsYou && prev != Some(AgentState::NeedsYou) {
                entered_needs_you.push(a);
            }
        }
        self.prev_states.retain(|id, _| scene.agents.contains_key(id));
        self.history.retain(|id, _| scene.agents.contains_key(id));

        for a in entered_needs_you {
            let body = if a.activity.is_empty() { "waiting for you".to_string() } else { a.activity.clone() };
            self.maybe_notify(&a.id.clone(), &format!("{} needs you", a.label), &body, now_ms, ctl);
        }

        // New Critical alerts.
        let mut fresh = Vec::new();
        let mut present = HashSet::new();
        for al in &scene.alerts {
            let key = (al.at_ms, al.code.clone(), al.agent.clone(), al.message.clone());
            if !self.seen_alerts.contains(&key) && al.level == AlertLevel::Critical {
                fresh.push(al.clone());
            }
            present.insert(key);
        }
        self.seen_alerts = present;
        for al in fresh {
            let who = al.agent.as_ref().and_then(|id| scene.agents.get(id)).map(|a| a.label.clone());
            let title = match &who {
                Some(l) => format!("office: {} ({})", al.code, l),
                None => format!("office: {}", al.code),
            };
            let key = al.agent.clone().unwrap_or_else(|| format!("alert:{}", al.code));
            self.maybe_notify(&key, &title, &al.message, now_ms, ctl);
        }
        self.dirty = true;
    }

    /// Rate-limited (1 per 10 s per key) notification that respects the `n` toggle.
    /// Returns true if a notification was sent.
    pub fn maybe_notify(&mut self, key: &str, title: &str, body: &str, now_ms: u64, ctl: &mut dyn Control) -> bool {
        if !self.notifications {
            return false;
        }
        if let Some(&last) = self.last_notify.get(key) {
            if now_ms < last + NOTIFY_INTERVAL_MS {
                return false;
            }
        }
        self.last_notify.insert(key.to_string(), now_ms);
        ctl.notify(title, body);
        true
    }

    fn move_selection(&mut self, scene: &Scene, delta: i32) {
        let rows = fmt::agent_rows(scene);
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        let cur = self.selected.as_ref().and_then(|s| rows.iter().position(|(id, _)| id == s));
        let next = match cur {
            None => 0,
            Some(i) => (i as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
        };
        self.selected = Some(rows[next].0.clone());
    }

    /// Preset names `t` cycles through: the standard order (only themes that exist or are
    /// configured as presets), then any extra configured presets.
    pub fn preset_cycle(&self, themes: &[String]) -> Vec<String> {
        let mut v: Vec<String> = PRESET_ORDER
            .iter()
            .filter(|n| **n == "procedural" || themes.iter().any(|t| t == *n) || self.presets.contains_key(**n))
            .map(|s| s.to_string())
            .collect();
        for k in self.presets.keys() {
            if !v.contains(k) {
                v.push(k.clone());
            }
        }
        v
    }

    pub fn preset_mix(&self, name: &str) -> ThemeMix {
        self.presets.get(name).cloned().unwrap_or_else(|| uniform_mix(name))
    }

    fn apply_mix(&mut self, mix: ThemeMix, view: &mut dyn ThemeHost, now_ms: u64, persist: bool) {
        view.set_mix(&mix);
        self.mix = mix;
        if persist {
            self.persist(now_ms);
        }
        self.dirty = true;
    }

    fn persist(&mut self, now_ms: u64) {
        if let Some(p) = self.config_path.clone() {
            if let Err(e) = persist_theme(&p, &self.mix) {
                self.toast(format!("could not save theme: {e}"), AlertLevel::Warn, now_ms);
            }
        }
    }

    fn cycle_theme(&mut self, view: &mut dyn ThemeHost, now_ms: u64) {
        let themes = view.themes();
        let cycle = self.preset_cycle(&themes);
        let idx = cycle.iter().position(|n| mix_eq(&self.preset_mix(n), &self.mix));
        let next = match idx {
            Some(i) => (i + 1) % cycle.len(),
            None => 0,
        };
        let name = cycle[next].clone();
        let mix = self.preset_mix(&name);
        self.apply_mix(mix, view, now_ms, true);
        self.toast(format!("theme: {name}"), AlertLevel::Info, now_ms);
    }

    fn mixer_step(&mut self, layer: usize, delta: i32, view: &mut dyn ThemeHost, now_ms: u64) {
        let mut themes = view.themes();
        if !themes.iter().any(|t| t == "procedural") {
            themes.insert(0, "procedural".into());
        }
        let cur = layer_get(&self.mix, layer).to_string();
        let i = themes.iter().position(|t| *t == cur).map(|i| i as i32).unwrap_or(-1);
        let n = themes.len() as i32;
        let next = ((i + delta) % n + n) % n;
        let mut mix = self.mix.clone();
        layer_set(&mut mix, layer, themes[next as usize].clone());
        self.apply_mix(mix, view, now_ms, false);
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        scene: &Scene,
        view: &mut dyn ThemeHost,
        ctl: &mut dyn Control,
        now_ms: u64,
    ) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }
        self.dirty = true;
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return Outcome::Quit;
        }

        // Popups capture keys first.
        match self.popup.clone() {
            Popup::None => {}
            Popup::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.popup = Popup::None;
                }
                return Outcome::Continue;
            }
            Popup::Detail(_) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.move_selection(scene, -1);
                        if let Some(id) = &self.selected {
                            self.popup = Popup::Detail(id.clone());
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.move_selection(scene, 1);
                        if let Some(id) = &self.selected {
                            self.popup = Popup::Detail(id.clone());
                        }
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::ConfirmCancel { id, label, pid } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.popup = Popup::None;
                        match scene.agents.get(&id) {
                            Some(a) => match ctl.cancel_agy(a, pid) {
                                Ok(()) => self.toast(format!("sent SIGTERM to {label} (pid {pid})"), AlertLevel::Info, now_ms),
                                Err(e) => self.toast(format!("cancel failed: {e}"), AlertLevel::Critical, now_ms),
                            },
                            None => self.toast(format!("{label} already left"), AlertLevel::Info, now_ms),
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::Mixer { layer } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Popup::Mixer { layer: layer.saturating_sub(1) },
                    KeyCode::Down | KeyCode::Char('j') => self.popup = Popup::Mixer { layer: (layer + 1).min(LAYERS.len() - 1) },
                    KeyCode::Left | KeyCode::Char('h') => self.mixer_step(layer, -1, view, now_ms),
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => self.mixer_step(layer, 1, view, now_ms),
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('T') | KeyCode::Char('q') => {
                        self.popup = Popup::None;
                        self.persist(now_ms);
                    }
                    _ => {}
                }
                return Outcome::Continue;
            }
        }

        match key.code {
            KeyCode::Char('q') => return Outcome::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(scene, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(scene, 1),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(scene, i32::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(scene, i32::MAX / 2),
            KeyCode::Enter => {
                if let Some(a) = self.selected_agent(scene) {
                    self.popup = Popup::Detail(a.id.clone());
                } else {
                    self.toast("no agent selected", AlertLevel::Info, now_ms);
                }
            }
            KeyCode::Char('t') => self.cycle_theme(view, now_ms),
            KeyCode::Char('T') => self.popup = Popup::Mixer { layer: 0 },
            KeyCode::Char('s') => self.sidebar = !self.sidebar,
            KeyCode::Char('n') => {
                self.notifications = !self.notifications;
                let s = if self.notifications { "notifications on" } else { "notifications off" };
                self.toast(s, AlertLevel::Info, now_ms);
            }
            KeyCode::Char('c') => {
                let latest = scene.alerts.iter().map(|a| a.at_ms).max().unwrap_or(0);
                self.alerts_cleared_at = Some(latest.max(self.alerts_cleared_at.unwrap_or(0)));
                self.toast("alerts cleared", AlertLevel::Info, now_ms);
            }
            KeyCode::Char('a') => self.permission(scene, true, ctl, now_ms),
            KeyCode::Char('d') => self.permission(scene, false, ctl, now_ms),
            KeyCode::Char('x') => match self.selected_agent(scene) {
                Some(a) if a.kind == AgentKind::Agy => match a.pid {
                    Some(pid) => self.popup = Popup::ConfirmCancel { id: a.id.clone(), label: a.label.clone(), pid },
                    None => self.toast(format!("{} has no router pid yet", a.label), AlertLevel::Info, now_ms),
                },
                Some(a) => {
                    let l = a.label.clone();
                    self.toast(format!("x cancels agy calls only ({l} is not one)"), AlertLevel::Info, now_ms)
                }
                None => self.toast("no agent selected", AlertLevel::Info, now_ms),
            },
            KeyCode::Char('o') => self.resume(scene, ctl, now_ms),
            KeyCode::Char('?') => self.popup = Popup::Help,
            _ => self.dirty = false,
        }
        Outcome::Continue
    }

    fn permission(&mut self, scene: &Scene, allow: bool, ctl: &mut dyn Control, now_ms: u64) {
        let verb = if allow { "approve" } else { "deny" };
        if !self.permissions_enabled {
            self.toast(
                format!("{verb} disabled: set permission_timeout_s > 0 and run `office install-hooks`"),
                AlertLevel::Warn,
                now_ms,
            );
            return;
        }
        let Some(a) = self.selected_agent(scene) else {
            self.toast("no agent selected", AlertLevel::Info, now_ms);
            return;
        };
        let Some(req) = a.pending_permission.clone() else {
            let l = a.label.clone();
            self.toast(format!("{l} has no pending permission"), AlertLevel::Info, now_ms);
            return;
        };
        let label = a.label.clone();
        match ctl.resolve_permission(a, &req, allow) {
            Ok(()) => self.toast(format!("{}d {label}", verb), AlertLevel::Info, now_ms),
            Err(e) => self.toast(format!("{verb} failed: {e}"), AlertLevel::Critical, now_ms),
        }
    }

    fn resume(&mut self, scene: &Scene, ctl: &mut dyn Control, now_ms: u64) {
        let Some(a) = self.selected_agent(scene) else {
            self.toast("no agent selected", AlertLevel::Info, now_ms);
            return;
        };
        // For a subagent, resume its parent session.
        let target = match a.kind {
            AgentKind::Claude => Some(a),
            AgentKind::ClaudeSub => a.parent.as_ref().and_then(|p| scene.agents.get(p)).or(Some(a)),
            _ => None,
        };
        let Some(t) = target else {
            let l = a.label.clone();
            self.toast(format!("o resumes Claude sessions only ({l} is not one)"), AlertLevel::Info, now_ms);
            return;
        };
        let Some(rid) = t.resume_id.clone() else {
            let l = t.label.clone();
            self.toast(format!("{l}: no session id to resume"), AlertLevel::Info, now_ms);
            return;
        };
        let cwd = t.cwd.clone();
        match ctl.resume_claude(cwd.as_deref(), &rid) {
            Ok(cmd) => self.toast(format!("copied: {cmd}"), AlertLevel::Info, now_ms),
            Err(e) => self.toast(format!("resume failed: {e}"), AlertLevel::Critical, now_ms),
        }
    }

    /// Mouse: left click on a sidebar row or on an agent in the office selects it.
    /// `hit` maps office pixel coordinates to an agent (Renderer::hit_test).
    pub fn handle_mouse(&mut self, ev: MouseEvent, scene: &Scene, hit: &dyn Fn(u32, u32) -> Option<AgentId>) -> bool {
        if ev.kind == MouseEventKind::Moved {
            let id = self.office_hit(ev.column, ev.row, hit);
            if id != self.hover {
                self.hover = id;
                return true;
            }
            return false;
        }
        if ev.kind != MouseEventKind::Down(MouseButton::Left) {
            return match ev.kind {
                MouseEventKind::ScrollUp if self.popup == Popup::None => {
                    self.move_selection(scene, -1);
                    true
                }
                MouseEventKind::ScrollDown if self.popup == Popup::None => {
                    self.move_selection(scene, 1);
                    true
                }
                _ => false,
            };
        }
        let (x, y) = (ev.column, ev.row);
        let r = self.layout.agent_rows;
        if self.sidebar && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
            if let Some(id) = self.layout.row_ids.get((y - r.y) as usize) {
                self.selected = Some(id.clone());
                self.dirty = true;
                return true;
            }
            return false;
        }
        if let Some(id) = self.office_hit(x, y, hit) {
            if scene.agents.contains_key(&id) || is_ambient(&id) {
                self.selected = Some(id);
                self.dirty = true;
                return true;
            }
        }
        false
    }

    /// The agent (or ambient character) under terminal cell (x, y) in the office, if any.
    fn office_hit(&self, x: u16, y: u16, hit: &dyn Fn(u32, u32) -> Option<AgentId>) -> Option<AgentId> {
        let o = self.layout.office;
        if x >= o.x && x < o.x + o.width && y >= o.y && y < o.y + o.height {
            let (fw, fh) = self.layout.font_px?;
            let px = (x - o.x) as u32 * fw as u32 + fw as u32 / 2;
            let py = (y - o.y) as u32 * fh as u32 + fh as u32 / 2;
            return hit(px, py);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;
    use crossterm::event::KeyEvent;
    use office_core::Alert;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    struct Harness {
        app: App,
        scene: Scene,
        view: FakeView,
        ctl: FakeControl,
    }
    impl Harness {
        fn new() -> Self {
            let cfg = office_core::config::Config::default();
            let mut app = App::new(&cfg, false);
            app.config_path = None;
            let scene = demo_scene();
            let mut ctl = FakeControl::default();
            app.observe(&scene, 1_000, &mut ctl);
            Harness { app, scene, view: FakeView::new(&["procedural", "modern", "cozy", "retro", "cyberpunk"]), ctl }
        }
        fn press(&mut self, k: KeyEvent) -> Outcome {
            self.app.handle_key(k, &self.scene, &mut self.view, &mut self.ctl, 5_000)
        }
        fn toast(&self) -> String {
            self.app.current_toast().map(|t| t.text.clone()).unwrap_or_default()
        }
    }

    #[test]
    fn quit_and_ctrl_c() {
        let mut h = Harness::new();
        assert_eq!(h.press(key('q')), Outcome::Quit);
        assert_eq!(h.press(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Outcome::Quit);
        // q inside a popup only closes it
        h.press(key('?'));
        assert_eq!(h.app.popup, Popup::Help);
        assert_eq!(h.press(key('q')), Outcome::Continue);
        assert_eq!(h.app.popup, Popup::None);
    }

    #[test]
    fn selection_moves_in_tree_order() {
        let mut h = Harness::new();
        let rows: Vec<String> = fmt::agent_rows(&h.scene).into_iter().map(|r| r.0).collect();
        assert_eq!(h.app.selected.as_ref(), Some(&rows[0]));
        h.press(key('j'));
        assert_eq!(h.app.selected.as_ref(), Some(&rows[1]));
        h.press(code(KeyCode::Down));
        assert_eq!(h.app.selected.as_ref(), Some(&rows[2]));
        h.press(key('k'));
        h.press(code(KeyCode::Up));
        h.press(code(KeyCode::Up)); // clamps at top
        assert_eq!(h.app.selected.as_ref(), Some(&rows[0]));
        h.press(key('G'));
        assert_eq!(h.app.selected.as_ref(), rows.last());
    }

    #[test]
    fn detail_popup_follows_selection() {
        let mut h = Harness::new();
        h.press(code(KeyCode::Enter));
        let first = h.app.selected.clone().unwrap();
        assert_eq!(h.app.popup, Popup::Detail(first));
        h.press(key('j'));
        assert_eq!(h.app.popup, Popup::Detail(h.app.selected.clone().unwrap()));
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
    }

    #[test]
    fn toggles() {
        let mut h = Harness::new();
        assert!(h.app.sidebar);
        h.press(key('s'));
        assert!(!h.app.sidebar);
        assert!(h.app.notifications);
        h.press(key('n'));
        assert!(!h.app.notifications);
        assert_eq!(h.toast(), "notifications off");
        h.press(key('c'));
        assert!(h.app.visible_alerts(&h.scene).is_empty());
    }

    #[test]
    fn theme_cycle_through_presets() {
        let mut h = Harness::new();
        let mut seen = vec![];
        for _ in 0..5 {
            h.press(key('t'));
            seen.push(h.app.mix.floor.clone());
            assert!(mix_eq(&h.view.mix, &h.app.mix));
        }
        assert_eq!(seen, vec!["modern", "cozy", "retro", "cyberpunk", "procedural"]);
        // Missing themes are skipped.
        let mut h = Harness::new();
        h.view = FakeView::new(&["procedural", "retro"]);
        h.press(key('t'));
        assert_eq!(h.app.mix.characters, "retro");
        h.press(key('t'));
        assert_eq!(h.app.mix.characters, "procedural");
    }

    #[test]
    fn theme_is_persisted_preserving_other_keys() {
        let dir = std::env::temp_dir().join(format!("office-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "fps = 30\n[theme]\nfloor = \"x\"\n").unwrap();
        let mut h = Harness::new();
        h.app.config_path = Some(path.clone());
        h.press(key('t'));
        let s = std::fs::read_to_string(&path).unwrap();
        let cfg: office_core::config::Config = toml::from_str(&s).unwrap();
        assert_eq!(cfg.fps, 30);
        assert_eq!(cfg.theme.floor, "modern");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixer_sets_individual_layers() {
        let mut h = Harness::new();
        h.press(key('T'));
        assert_eq!(h.app.popup, Popup::Mixer { layer: 0 });
        h.press(code(KeyCode::Right));
        assert_eq!(h.app.mix.floor, "modern");
        h.press(key('j'));
        h.press(key('j'));
        h.press(code(KeyCode::Left)); // furniture: procedural -> wraps to cyberpunk
        assert_eq!(h.app.mix.furniture, "cyberpunk");
        assert_eq!(h.app.mix.walls, "procedural");
        assert!(mix_eq(&h.view.mix, &h.app.mix));
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
    }

    #[test]
    fn approve_deny_gating() {
        let mut h = Harness::new();
        select(&mut h.app, "cc:office");
        // permission_timeout_s == 0 -> hint, no call
        h.press(key('a'));
        assert!(h.toast().contains("disabled"), "{}", h.toast());
        assert!(h.ctl.calls.is_empty());
        h.app.permissions_enabled = true;
        h.press(key('a'));
        assert_eq!(h.ctl.calls, vec!["perm perm-1 allow".to_string()]);
        h.press(key('d'));
        assert_eq!(h.ctl.calls.last().unwrap(), "perm perm-1 deny");
        // agent without a pending permission
        select(&mut h.app, "cc:dice");
        h.press(key('a'));
        assert!(h.toast().contains("no pending permission"));
        assert_eq!(h.ctl.calls.len(), 2);
    }

    #[test]
    fn cancel_agy_requires_confirmation() {
        let mut h = Harness::new();
        select(&mut h.app, "cc:dice");
        h.press(key('x'));
        assert!(h.toast().contains("agy calls only"));
        select(&mut h.app, "agy:4242");
        h.press(key('x'));
        assert!(matches!(&h.app.popup, Popup::ConfirmCancel { pid: 4242, .. }));
        h.press(key('n'));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.ctl.calls.is_empty());
        h.press(key('x'));
        h.press(key('y'));
        assert_eq!(h.ctl.calls, vec!["cancel 4242".to_string()]);
        assert!(h.toast().contains("SIGTERM"));
    }

    #[test]
    fn resume_uses_session_or_parent() {
        let mut h = Harness::new();
        select(&mut h.app, "cc-sub:dice/1");
        h.press(key('o'));
        assert_eq!(h.ctl.calls, vec!["resume /tmp/dice sess-dice".to_string()]);
        assert!(h.toast().starts_with("copied: "));
        select(&mut h.app, "agy:4242");
        h.press(key('o'));
        assert!(h.toast().contains("Claude sessions only"));
    }

    #[test]
    fn control_errors_become_toasts() {
        let mut h = Harness::new();
        h.ctl.fail = true;
        select(&mut h.app, "cc:dice");
        h.press(key('o'));
        assert!(h.toast().contains("resume failed"));
        assert_eq!(h.app.current_toast().unwrap().level, AlertLevel::Critical);
    }

    #[test]
    fn notifications_rate_limited_and_toggled() {
        let cfg = office_core::config::Config::default();
        let mut app = App::new(&cfg, false);
        app.config_path = None;
        let mut ctl = FakeControl::default();
        let mut scene = demo_scene();
        app.observe(&scene, 1_000, &mut ctl);
        // cc:office starts in NeedsYou -> one notification
        assert_eq!(ctl.notes.len(), 1, "{:?}", ctl.notes);
        // flip out and back in within 10 s: suppressed
        scene.agents.get_mut("cc:office").unwrap().state = AgentState::Running;
        app.observe(&scene, 2_000, &mut ctl);
        scene.agents.get_mut("cc:office").unwrap().state = AgentState::NeedsYou;
        app.observe(&scene, 3_000, &mut ctl);
        assert_eq!(ctl.notes.len(), 1);
        // after 10 s: allowed again
        scene.agents.get_mut("cc:office").unwrap().state = AgentState::Running;
        app.observe(&scene, 12_000, &mut ctl);
        scene.agents.get_mut("cc:office").unwrap().state = AgentState::NeedsYou;
        app.observe(&scene, 12_500, &mut ctl);
        assert_eq!(ctl.notes.len(), 2);
        // new Critical alert notifies once; Warn does not
        scene.alerts.push(Alert { at_ms: 13_000, level: AlertLevel::Warn, agent: None, code: "loop".into(), message: "m".into() });
        scene.alerts.push(Alert {
            at_ms: 13_000,
            level: AlertLevel::Critical,
            agent: Some("agy:4242".into()),
            code: "timeout".into(),
            message: "agy timed out".into(),
        });
        app.observe(&scene, 13_000, &mut ctl);
        assert_eq!(ctl.notes.len(), 3);
        assert!(ctl.notes[2].0.contains("timeout"));
        app.observe(&scene, 30_000, &mut ctl); // already seen
        assert_eq!(ctl.notes.len(), 3);
        // n toggle off: nothing
        app.notifications = false;
        scene.alerts.push(Alert { at_ms: 31_000, level: AlertLevel::Critical, agent: None, code: "x".into(), message: "m".into() });
        app.observe(&scene, 31_000, &mut ctl);
        assert_eq!(ctl.notes.len(), 3);
    }

    #[test]
    fn selection_repairs_when_agent_leaves() {
        let mut h = Harness::new();
        select(&mut h.app, "agy:4242");
        h.press(code(KeyCode::Enter));
        h.scene.agents.remove("agy:4242");
        h.app.observe(&h.scene, 6_000, &mut h.ctl);
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.scene.agents.contains_key(h.app.selected.as_ref().unwrap()));
    }

    #[test]
    fn activity_history_records_changes() {
        let mut h = Harness::new();
        h.scene.agents.get_mut("cc:dice").unwrap().activity = "Edit lib.rs".into();
        h.app.observe(&h.scene, 6_000, &mut h.ctl);
        h.app.observe(&h.scene, 7_000, &mut h.ctl); // unchanged: no dup
        let hist: Vec<_> = h.app.history["cc:dice"].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(hist, vec!["Read src/main.rs", "Edit lib.rs"]);
    }

    #[test]
    fn mouse_selects_rows_and_office_agents() {
        let mut h = Harness::new();
        h.app.layout.agent_rows = Rect::new(60, 3, 40, 5);
        h.app.layout.row_ids = vec!["cc:dice".into(), "agy:4242".into()];
        h.app.layout.office = Rect::new(0, 1, 58, 20);
        h.app.layout.font_px = Some((10, 20));
        let click = |c, r| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };
        assert!(h.app.handle_mouse(click(65, 4), &h.scene, &|_, _| None));
        assert_eq!(h.app.selected.as_deref(), Some("agy:4242"));
        // office click at cell (3, 2) -> pixel centre (35, 50) relative to office
        let hit = |x: u32, y: u32| if (x, y) == (35, 50) { Some("cc:office".to_string()) } else { None };
        assert!(h.app.handle_mouse(click(3, 3), &h.scene, &hit));
        assert_eq!(h.app.selected.as_deref(), Some("cc:office"));
        assert!(!h.app.handle_mouse(click(4, 3), &h.scene, &hit));
        // the cat / CEO are selectable and hoverable, and survive selection repair
        let cat = |x: u32, y: u32| if (x, y) == (35, 50) { Some("office:cat".to_string()) } else { None };
        let mv = MouseEvent { kind: MouseEventKind::Moved, column: 3, row: 3, modifiers: KeyModifiers::NONE };
        assert!(h.app.handle_mouse(mv, &h.scene, &cat));
        assert_eq!(h.app.hover.as_deref(), Some("office:cat"));
        assert!(!h.app.handle_mouse(mv, &h.scene, &cat), "same hover: no redraw");
        assert!(h.app.handle_mouse(click(3, 3), &h.scene, &cat));
        assert_eq!(h.app.selected.as_deref(), Some("office:cat"));
        h.app.observe(&h.scene, 9_000, &mut h.ctl);
        assert_eq!(h.app.selected.as_deref(), Some("office:cat"));
    }
}
