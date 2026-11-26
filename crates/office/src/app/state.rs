//! App state machine: selection, popups, toasts, key/mouse handling, notifications, theme cycling.
//! Everything here is pure over (Scene, ThemeHost, Control) so it can be tested without a tty.
use super::chime::{Chime, Chimer};
use super::fmt;
use super::panels::{self, SideTab, StateNote};
use super::reader::{self, ReaderTab};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use office_core::{config::{ShowConfig, ThemeMix}, model::AgentKind, Agent, AgentId, AgentState, AlertLevel, Scene};
use ratatui::layout::Rect;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

/// Theme access needed by the key handler (implemented by render::Renderer and test fakes).
pub trait ThemeHost {
    fn themes(&self) -> Vec<String>;
    fn set_mix(&mut self, mix: &ThemeMix);
    /// Apply the display toggles (names, bubbles, zones, ...).
    fn set_show(&mut self, _show: &ShowConfig) {}
}

/// Side-effecting control actions (real: sources::control; demo: synthetic events; tests: fakes).
pub trait Control {
    fn cancel_agy(&mut self, agent: &Agent, pid: u32) -> anyhow::Result<()>;
    fn resolve_permission(&mut self, agent: &Agent, request_id: &str, allow: bool) -> anyhow::Result<()>;
    fn resume_claude(&mut self, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String>;
    fn notify(&mut self, title: &str, body: &str);
    fn play_sound(&mut self, _chime: Chime) {}
    /// Put text on the system clipboard (non-blocking).
    fn copy_text(&mut self, _text: &str) -> anyhow::Result<()> {
        anyhow::bail!("clipboard not available")
    }
    /// Save a file into the downloads folder; returns the path to show.
    fn save_file(&mut self, _name: &str, _content: &str) -> anyhow::Result<String> {
        anyhow::bail!("saving not available")
    }
    fn kill_agent(&mut self, agent: &Agent, pid: u32) -> anyhow::Result<()> {
        self.cancel_agy(agent, pid)
    }
    fn reap_orphans(&mut self) -> anyhow::Result<(usize, u32)> {
        Ok((0, 0))
    }
    fn resume_agent(&mut self, _agent: &Agent, cwd: Option<&str>, session_id: &str) -> anyhow::Result<String> {
        self.resume_claude(cwd, session_id)
    }
    fn dispatch_task(&mut self, _target: &str, _cwd: &str, _prompt: &str) -> anyhow::Result<String> {
        anyhow::bail!("dispatch not available")
    }
    fn followup_session(&mut self, _agent: &Agent, _prompt: &str) -> anyhow::Result<String> {
        anyhow::bail!("follow-up not available")
    }
    fn prune_brain(&mut self) -> anyhow::Result<String> {
        anyhow::bail!("prune not available")
    }
    fn resume_auto(&self, _job: &str) -> Result<String, String> {
        Err("rosso-auto control not available".into())
    }
    fn abort_auto(&self, _job: &str) -> Result<String, String> {
        Err("rosso-auto control not available".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleTarget {
    pub id: AgentId,
    pub label: String,
    pub pid: u32,
    pub rss_mb: u32,
    pub idle_ms: u64,
    pub is_orphan: bool,
}

pub const DISPATCH_TARGETS: &[(&str, &str)] = &[
    ("agy:worker", "agy worker (edit)"),
    ("agy:architect", "agy architect (plan)"),
    ("agy:supervisor", "agy supervisor (review)"),
    ("agy:tester", "agy tester (verify)"),
    ("agy:fallback", "agy fallback (quick)"),
    ("agy:manager", "agy manager (coordinate)"),
    ("agy:git", "agy git (branch/commit)"),
    ("agy-cli", "agy CLI (standalone)"),
    ("claude", "claude (background -p)"),
    ("claude-win", "claude (Ghostty window)"),
    ("rosso-auto:council", "rosso-auto council (full pipeline)"),
    ("rosso-auto:simple", "rosso-auto simple (fast pipeline)"),
];

pub const ACTIONS: &[(&str, &str)] = &[
    ("p", "Dispatch new task (agy / claude)"),
    ("f", "Reply / follow-up to selected agent"),
    ("K", "Free RAM — kill idle agents"),
    ("R", "Reap orphan agy processes"),
    ("A", "Approve all pending permissions"),
    ("i", "Open Boss Inbox"),
    ("o", "Open selected session in Ghostty"),
    ("B", "Prune old agy brain directories"),
];

/// Candidates for the Free RAM dialog: idle/done agents with live PIDs and no active children,
/// sorted by RSS descending, then idle duration descending, then agent id ascending.
pub fn idle_kill_candidates(scene: &Scene, now_ms: u64) -> Vec<IdleTarget> {
    let mut out: Vec<IdleTarget> = scene
        .agents
        .values()
        .filter_map(|a| {
            let pid = a.pid.filter(|&p| p > 1)?;
            if !matches!(a.state, AgentState::Idle | AgentState::Done) {
                return None;
            }
            let has_active_child = scene.agents.values().any(|c| {
                c.parent.as_deref() == Some(a.id.as_str())
                    && !matches!(c.state, AgentState::Idle | AgentState::Done | AgentState::Leaving)
            });
            if has_active_child {
                return None;
            }
            Some(IdleTarget {
                id: a.id.clone(),
                label: a.label.clone(),
                pid,
                rss_mb: a.rss_mb.unwrap_or(0),
                idle_ms: now_ms.saturating_sub(a.last_event_ms),
                is_orphan: a.id.starts_with("ag-orphan:"),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.rss_mb
            .cmp(&a.rss_mb)
            .then_with(|| b.idle_ms.cmp(&a.idle_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

pub const PRESET_ORDER: [&str; 11] = [
    "procedural",
    "modern",
    "cozy",
    "midnight",
    "tokyo-night",
    "nord",
    "retro",
    "retro-1bit",
    "retro-amber",
    "retro-gameboy",
    "cyberpunk",
];
pub const LAYERS: [&str; 5] = ["floor", "walls", "furniture", "decor", "characters"];
pub const NOTIFY_INTERVAL_MS: u64 = 10_000;
pub const TOAST_MS: u64 = 4_000;
const HISTORY_LEN: usize = 12;

/// A theme mix with value equality (ThemeMix itself has none).
#[derive(Debug, Clone)]
pub struct MixSnap(pub ThemeMix);
impl PartialEq for MixSnap {
    fn eq(&self, o: &Self) -> bool {
        mix_eq(&self.0, &o.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    None,
    Help,
    Detail(AgentId),
    ConfirmCancel { id: AgentId, label: String, pid: u32 },
    Mixer { layer: usize },
    /// Theme picker: `names` are the presets, followed by a "custom mix…" row; `orig` is restored on Esc.
    Theme { sel: usize, names: Vec<String>, orig: MixSnap },
    /// The View menu: display toggles for the office overlays.
    View { sel: usize },
    /// Hand-offs, newest first: everything, or only what one agent sent and received.
    /// With `inbox` set, only reports addressed to the boss (`to == None`).
    Reports { sel: usize, only: Option<AgentId>, inbox: bool },
    /// One hand-off in full (brief + report); Esc goes back to the list.
    ReportView { id: u64, only: Option<AgentId>, inbox: bool },
    /// Free RAM dialog: multi-select idle/done agents and orphans to stop.
    FreeRam { sel: usize, targets: Vec<IdleTarget>, checked: Vec<bool> },
    /// Dispatch a new task or follow-up prompt to a session.
    Dispatch { field: usize, target_idx: usize, cwd: String, prompt: String, followup: Option<(AgentId, String)> },
    /// Command palette / quick actions menu.
    Actions { sel: usize },
    /// Confirmation before approving all pending permissions.
    ConfirmApproveAll { items: Vec<(AgentId, String, String)> },
    /// Confirmation before aborting a `rosso-auto` job.
    ConfirmAbortAuto { job: String, phase: String },
}

/// Reports newest first, optionally only those sent or received by `only`.
pub fn report_list<'a>(scene: &'a Scene, only: Option<&str>) -> Vec<&'a office_core::Report> {
    scene
        .reports
        .iter()
        .rev()
        .filter(|r| only.is_none_or(|id| r.from == id || r.to.as_deref() == Some(id)))
        .collect()
}

/// The list a Reports popup shows: the boss inbox, or everything / one agent's hand-offs.
pub fn report_list_for<'a>(scene: &'a Scene, only: Option<&str>, inbox: bool) -> Vec<&'a office_core::Report> {
    if inbox {
        scene.reports.iter().rev().filter(|r| r.to.is_none()).collect()
    } else {
        report_list(scene, only)
    }
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
    /// Area of the alerts list body.
    pub alert_rows: Rect,
    /// Associated agent ids for visible alert rows.
    pub alert_agent_ids: Vec<Option<AgentId>>,
    /// Currently open modal popup rect (for click-outside-to-close).
    pub popup: Rect,
    /// The boss-inbox notification card (empty when not showing).
    pub card: Rect,
    /// Maximum scroll offset for the currently open detail popup.
    pub detail_max_scroll: u16,
    /// Terminal font size in pixels (w, h) when an image protocol is active.
    pub font_px: Option<(u16, u16)>,
    /// The sidebar's tab bar (empty when not drawn).
    pub side_tabs: Rect,
    /// Clickable Free RAM button in the header/sidebar (empty when not drawn).
    pub free_ram_btn: Rect,
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

/// Write `theme = {..}` into the TOML config atomically, keeping every other key and failing safely
/// if an existing config file cannot be read or parsed.
pub fn persist_config(path: &std::path::Path, mix: &ThemeMix, show: &ShowConfig, notifications: bool, sound: bool) -> anyhow::Result<()> {
    let mut table: toml::Table = match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml::Table>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e.into()),
    };
    table.insert("theme".into(), toml::Value::try_from(mix)?);
    table.insert("show".into(), toml::Value::try_from(show)?);
    table.insert("notifications".into(), toml::Value::Boolean(notifications));
    table.insert("sound".into(), toml::Value::Boolean(sound));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.office-tmp");
    std::fs::write(&tmp, toml::to_string_pretty(&table)?)?;
    std::fs::rename(&tmp, path)?;
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
    pub detail_scroll: u16,
    pub sidebar: bool,
    pub notifications: bool,
    pub sound: bool,
    chimer: Chimer,
    pub toasts: Vec<Toast>,
    /// Alerts at or before this time are hidden (the `c` key).
    pub alerts_cleared_at: Option<u64>,
    pub permissions_enabled: bool,
    pub demo: bool,
    /// Where `t`/`T` persist the theme mix; None = don't persist (tests).
    pub config_path: Option<PathBuf>,
    pub mix: ThemeMix,
    pub show: ShowConfig,
    pub presets: BTreeMap<String, ThemeMix>,
    /// Recent activity per agent (time, text), newest last.
    pub history: HashMap<AgentId, VecDeque<(u64, String)>>,
    prev_states: HashMap<AgentId, AgentState>,
    seen_alerts: HashSet<(u64, String, Option<AgentId>, String)>,
    last_notify: HashMap<String, u64>,
    last_click: Option<(u64, AgentId)>,
    /// Highest report id the boss has seen or dismissed.
    pub inbox_seen: u64,
    /// Highest boss report id already announced through the notify path.
    inbox_notified: u64,
    /// Set by the first `observe`: reports present then count as seen.
    inbox_init: bool,
    pub layout: LayoutCache,
    /// Set whenever the UI needs a redraw independent of scene changes.
    pub dirty: bool,
    /// Theme restore requested where no view is at hand (mouse); the event loop applies it.
    pub pending_mix: Option<ThemeMix>,
    /// Which sidebar tab is showing (keys 1-4).
    pub side_tab: SideTab,
    pub jobs_sel: usize,
    pub feed_sel: usize,
    /// Notable agent state changes for the Activity tab.
    pub state_log: VecDeque<StateNote>,
    /// Cost / active-agent history for the Stats tab.
    pub samples: panels::Samples,
    /// The report reader's current tab.
    pub reader_tab: ReaderTab,
}

impl App {
    pub fn new(cfg: &office_core::config::Config, demo: bool) -> Self {
        App {
            selected: None,
            hover: None,
            popup: Popup::None,
            detail_scroll: 0,
            sidebar: cfg.sidebar,
            notifications: cfg.notifications,
            sound: cfg.sound,
            chimer: Chimer::default(),
            toasts: Vec::new(),
            alerts_cleared_at: None,
            permissions_enabled: demo || cfg.permission_timeout_s > 0,
            demo,
            config_path: Some(office_core::paths::config_file()),
            mix: cfg.theme.clone(),
            show: cfg.show.clone(),
            presets: cfg.presets.clone(),
            history: HashMap::new(),
            prev_states: HashMap::new(),
            seen_alerts: HashSet::new(),
            last_notify: HashMap::new(),
            last_click: None,
            inbox_seen: 0,
            inbox_notified: 0,
            inbox_init: false,
            layout: LayoutCache::default(),
            dirty: true,
            pending_mix: None,
            side_tab: SideTab::Agents,
            jobs_sel: 0,
            feed_sel: 0,
            state_log: VecDeque::new(),
            samples: panels::Samples::default(),
            reader_tab: ReaderTab::Summary,
        }
    }

    /// Housekeeping tick: sample history for the Stats tab (every 10 s).
    pub fn tick(&mut self, scene: &Scene, now_ms: u64) {
        if self.samples.sample(scene, now_ms) && self.side_tab == SideTab::Stats {
            self.dirty = true;
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
            .filter(|a| self.alerts_cleared_at.is_none_or(|c| a.at_ms > c))
            .collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
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
        match &mut self.popup {
            Popup::Detail(id) | Popup::ConfirmCancel { id, .. } => {
                if !scene.agents.contains_key(id) {
                    self.popup = Popup::None;
                }
            }
            Popup::FreeRam { sel, targets, checked } => {
                let mut keep_targets = Vec::with_capacity(targets.len());
                let mut keep_checked = Vec::with_capacity(targets.len());
                for (i, t) in targets.drain(..).enumerate() {
                    if scene.agents.contains_key(&t.id) {
                        keep_targets.push(t);
                        keep_checked.push(checked.get(i).copied().unwrap_or(true));
                    }
                }
                *targets = keep_targets;
                *checked = keep_checked;
                *sel = (*sel).min(targets.len().saturating_sub(1));
            }
            _ => {}
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
            if prev.is_some_and(|p| p != a.state) && panels::notable(a.state) {
                self.state_log.push_back(StateNote { at_ms: now_ms, label: a.label.clone(), state: a.state });
                while self.state_log.len() > panels::MAX_STATE_NOTES {
                    self.state_log.pop_front();
                }
            }
            if a.state == AgentState::NeedsYou && prev != Some(AgentState::NeedsYou) {
                entered_needs_you.push(a);
            }
        }
        self.prev_states.retain(|id, _| scene.agents.contains_key(id));
        self.history.retain(|id, _| scene.agents.contains_key(id));
        self.last_notify.retain(|k, _| k.starts_with("alert:") || scene.agents.contains_key(k));

        for a in entered_needs_you {
            let body = if a.activity.is_empty() { "waiting for you".to_string() } else { a.activity.clone() };
            self.maybe_notify(&a.id.clone(), &format!("{} needs you", a.label), &body, now_ms, ctl);
            self.chime(Chime::NeedsYou, now_ms, ctl);
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
            self.chime(Chime::Error, now_ms, ctl);
        }
        self.observe_inbox(scene, now_ms, ctl);
        self.dirty = true;
    }

    fn observe_inbox(&mut self, scene: &Scene, now_ms: u64, ctl: &mut dyn Control) {
        let top = scene.reports.iter().filter(|r| r.to.is_none()).map(|r| r.id).max().unwrap_or(0);
        if !self.inbox_init {
            self.inbox_init = true;
            self.inbox_seen = top;
            self.inbox_notified = top;
            return;
        }
        let fresh: Vec<office_core::Report> =
            scene.reports.iter().filter(|r| r.to.is_none() && r.id > self.inbox_notified).cloned().collect();
        self.inbox_notified = self.inbox_notified.max(top);
        for r in fresh {
            let title = format!("{} → you", r.from_label);
            self.maybe_notify(&format!("inbox:{}", r.id), &title, &r.title, now_ms, ctl);
            self.chime(if r.title.starts_with('⚠') { Chime::Error } else { Chime::Inbox }, now_ms, ctl);
        }
    }

    /// Reports addressed to the boss with an id above `inbox_seen` (none until `observe` ran once).
    pub fn unread<'a>(&self, scene: &'a Scene) -> Vec<&'a office_core::Report> {
        if !self.inbox_init {
            return Vec::new();
        }
        scene.reports.iter().filter(|r| r.to.is_none() && r.id > self.inbox_seen).collect()
    }

    pub fn unread_count(&self, scene: &Scene) -> usize {
        self.unread(scene).len()
    }

    /// The newest unseen boss report: what the notification card shows.
    pub fn card_report<'a>(&self, scene: &'a Scene) -> Option<&'a office_core::Report> {
        self.unread(scene).into_iter().max_by_key(|r| r.id)
    }

    fn mark_seen(&mut self, id: u64) {
        self.inbox_seen = self.inbox_seen.max(id);
    }

    fn dismiss_card(&mut self, scene: &Scene) {
        let top = scene.reports.iter().filter(|r| r.to.is_none()).map(|r| r.id).max().unwrap_or(0);
        self.mark_seen(top);
    }

    fn open_report(&mut self, id: u64, only: Option<AgentId>, inbox: bool) {
        self.mark_seen(id);
        self.detail_scroll = 0;
        self.popup = Popup::ReportView { id, only, inbox };
    }

    /// Play a chime if sound is on and the 3 s rate limit allows (independent of the `n` toggle).
    fn chime(&mut self, chime: Chime, now_ms: u64, ctl: &mut dyn Control) {
        if self.chimer.allow(self.sound, chime, now_ms) {
            ctl.play_sound(chime);
        }
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

    fn jump_next_attention(&mut self, scene: &Scene, now_ms: u64) {
        let rows: Vec<AgentId> = fmt::agent_rows(scene).into_iter().map(|(id, _)| id).collect();
        let pick_group = |pred: &dyn Fn(AgentState) -> bool| -> Option<AgentId> {
            let matches: Vec<&AgentId> = rows.iter().filter(|id| scene.agents.get(*id).is_some_and(|a| pred(a.state))).collect();
            if matches.is_empty() {
                return None;
            }
            let next_idx = self
                .selected
                .as_ref()
                .and_then(|sel| matches.iter().position(|id| *id == sel))
                .map(|i| (i + 1) % matches.len())
                .unwrap_or(0);
            Some(matches[next_idx].clone())
        };
        let target = pick_group(&|s| s == AgentState::NeedsYou)
            .or_else(|| pick_group(&|s| s == AgentState::Error))
            .or_else(|| {
                pick_group(&|s| {
                    matches!(
                        s,
                        AgentState::Typing
                            | AgentState::Running
                            | AgentState::Reading
                            | AgentState::Thinking
                            | AgentState::Delegating
                    )
                })
            });
        if let Some(id) = target {
            self.selected = Some(id);
        } else {
            self.toast("no active or alerting agents", AlertLevel::Info, now_ms);
        }
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
    /// configured as presets), then any extra configured presets, then any other discovered themes.
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
        for t in themes {
            if !v.contains(t) {
                v.push(t.clone());
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
            if let Err(e) = persist_config(&p, &self.mix, &self.show, self.notifications, self.sound) {
                self.toast(format!("could not save settings: {e}"), AlertLevel::Warn, now_ms);
            }
        }
    }

    fn open_theme_picker(&mut self, view: &mut dyn ThemeHost) {
        let names = self.preset_cycle(&view.themes());
        let sel = names.iter().position(|n| mix_eq(&self.preset_mix(n), &self.mix)).unwrap_or(names.len());
        self.popup = Popup::Theme { sel, names, orig: MixSnap(self.mix.clone()) };
    }

    /// Move the picker highlight and preview that entry live (the custom row leaves the mix alone).
    fn theme_pick(&mut self, sel: usize, names: Vec<String>, orig: MixSnap, view: &mut dyn ThemeHost, now_ms: u64) {
        let sel = sel.min(names.len());
        if let Some(n) = names.get(sel) {
            let mix = self.preset_mix(n);
            self.apply_mix(mix, view, now_ms, false);
        }
        self.popup = Popup::Theme { sel, names, orig };
        self.dirty = true;
    }

    fn close_theme_picker(&mut self, orig: MixSnap, view: &mut dyn ThemeHost, now_ms: u64) {
        self.apply_mix(orig.0, view, now_ms, false);
        self.popup = Popup::None;
    }

    fn cycle_theme(&mut self, delta: i32, view: &mut dyn ThemeHost, now_ms: u64) {
        let themes = view.themes();
        let cycle = self.preset_cycle(&themes);
        let idx = cycle.iter().position(|n| mix_eq(&self.preset_mix(n), &self.mix));
        let n = cycle.len() as i32;
        let next = match idx {
            Some(i) => ((i as i32 + delta) % n + n) % n,
            None if delta < 0 => n - 1,
            None => 0,
        } as usize;
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

    fn set_show_item(&mut self, i: usize, v: bool, view: &mut dyn ThemeHost) {
        self.show.set(i, v);
        view.set_show(&self.show);
        self.dirty = true;
    }

    fn set_show_all(&mut self, show: ShowConfig, view: &mut dyn ThemeHost) {
        self.show = show;
        view.set_show(&self.show);
        self.dirty = true;
    }

    fn default_cwd(&self, scene: &Scene) -> String {
        self.selected_agent(scene)
            .and_then(|a| a.cwd.as_deref())
            .filter(|c| !c.is_empty())
            .or_else(|| scene.agents.values().find_map(|a| a.cwd.as_deref().filter(|c| !c.is_empty())))
            .map(ToString::to_string)
            .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".into())
    }

    fn open_free_ram(&mut self, scene: &Scene, now_ms: u64) {
        let targets = idle_kill_candidates(scene, now_ms);
        let checked = vec![true; targets.len()];
        if targets.is_empty() && scene.router_extra.orphans == 0 {
            self.popup = Popup::None;
            self.toast("no idle killable agents", AlertLevel::Info, now_ms);
        } else {
            self.popup = Popup::FreeRam { sel: 0, targets, checked };
        }
    }

    fn open_followup(&mut self, scene: &Scene, id: &str, now_ms: u64) {
        let Some(a) = scene.agents.get(id) else {
            self.toast("agent is no longer in the office", AlertLevel::Info, now_ms);
            return;
        };
        let target = match a.kind {
            AgentKind::ClaudeSub => a.parent.as_ref().and_then(|p| scene.agents.get(p)).unwrap_or(a),
            _ => a,
        };
        if target.resume_id.as_deref().is_none_or(str::is_empty) {
            let l = target.label.clone();
            self.toast(format!("{l}: no session id to reply to"), AlertLevel::Info, now_ms);
            return;
        }
        let cwd = target
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| self.default_cwd(scene));
        self.popup = Popup::Dispatch {
            field: 2,
            target_idx: 0,
            cwd,
            prompt: String::new(),
            followup: Some((target.id.clone(), target.label.clone())),
        };
    }

    fn open_approve_all(&mut self, scene: &Scene, now_ms: u64) {
        if !self.permissions_enabled {
            self.toast(
                "approve disabled: set permission_timeout_s > 0 and run `office install-hooks`",
                AlertLevel::Warn,
                now_ms,
            );
            return;
        }
        let mut items: Vec<(AgentId, String, String)> = scene
            .agents
            .values()
            .filter_map(|a| a.pending_permission.clone().map(|req| (a.id.clone(), a.label.clone(), req)))
            .collect();
        items.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        if items.is_empty() {
            self.toast("no pending permissions", AlertLevel::Info, now_ms);
        } else {
            self.popup = Popup::ConfirmApproveAll { items };
        }
    }

    fn reap_orphans_with_toast(&mut self, ctl: &mut dyn Control, now_ms: u64) {
        match ctl.reap_orphans() {
            Ok((0, _)) => self.toast("no orphan agy processes", AlertLevel::Info, now_ms),
            Ok((n, mb)) => self.toast(format!("reaped {n} orphan agy (~{mb} MB)"), AlertLevel::Info, now_ms),
            Err(e) => self.toast(format!("reap failed: {e}"), AlertLevel::Critical, now_ms),
        }
    }

    fn run_action(&mut self, act: &str, scene: &Scene, ctl: &mut dyn Control, now_ms: u64) {
        match act {
            "p" => {
                self.popup = Popup::Dispatch {
                    field: 2,
                    target_idx: 0,
                    cwd: self.default_cwd(scene),
                    prompt: String::new(),
                    followup: None,
                };
            }
            "f" => match self.selected_agent(scene).map(|a| a.id.clone()) {
                Some(id) => self.open_followup(scene, &id, now_ms),
                None => self.toast("no agent selected", AlertLevel::Info, now_ms),
            },
            "K" => self.open_free_ram(scene, now_ms),
            "R" => self.reap_orphans_with_toast(ctl, now_ms),
            "A" => self.open_approve_all(scene, now_ms),
            "i" => {
                if scene.reports.iter().any(|r| r.to.is_none()) {
                    self.popup = Popup::Reports { sel: 0, only: None, inbox: true };
                } else {
                    self.toast("inbox is empty", AlertLevel::Info, now_ms);
                }
            }
            "o" => self.resume(scene, ctl, now_ms),
            "B" => match ctl.prune_brain() {
                Ok(msg) => self.toast(msg, AlertLevel::Info, now_ms),
                Err(e) => self.toast(format!("prune failed: {e}"), AlertLevel::Critical, now_ms),
            },
            _ => {}
        }
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
                let max = self.layout.detail_max_scroll;
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
                        self.popup = Popup::None;
                        self.detail_scroll = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.detail_scroll = self.detail_scroll.saturating_add(1).min(max),
                    KeyCode::Up | KeyCode::Char('k') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => self.detail_scroll = self.detail_scroll.saturating_add(8).min(max),
                    KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(8),
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::Detail(_) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.move_selection(scene, -1);
                        self.detail_scroll = 0;
                        if let Some(id) = &self.selected {
                            self.popup = Popup::Detail(id.clone());
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.move_selection(scene, 1);
                        self.detail_scroll = 0;
                        if let Some(id) = &self.selected {
                            self.popup = Popup::Detail(id.clone());
                        }
                    }
                    KeyCode::PageDown => {
                        self.detail_scroll = self.detail_scroll.saturating_add(6).min(self.layout.detail_max_scroll);
                    }
                    KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(6),
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.detail_scroll = self.detail_scroll.saturating_add(6).min(self.layout.detail_max_scroll);
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.detail_scroll = self.detail_scroll.saturating_sub(6);
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                        self.popup = Popup::None;
                        self.detail_scroll = 0;
                    }
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::ConfirmCancel { id, label, pid } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.popup = Popup::None;
                        match scene.agents.get(&id) {
                            Some(a) => match ctl.kill_agent(a, pid) {
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
            Popup::Reports { sel, only, inbox } => {
                let n = report_list_for(scene, only.as_deref(), inbox).len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Popup::Reports { sel: sel.saturating_sub(1), only, inbox },
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.popup = Popup::Reports { sel: (sel + 1).min(n.saturating_sub(1)), only, inbox }
                    }
                    KeyCode::Char('a') => self.popup = Popup::Reports { sel: 0, only: None, inbox: false },
                    KeyCode::Char('i') => self.popup = Popup::Reports { sel: 0, only: None, inbox: true },
                    KeyCode::Enter => {
                        let id = report_list_for(scene, only.as_deref(), inbox).get(sel).map(|r| r.id);
                        if let Some(id) = id {
                            self.open_report(id, only, inbox);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('r') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::ReportView { id, only, inbox } => {
                let list_ids: Vec<u64> = report_list_for(scene, only.as_deref(), inbox).iter().map(|r| r.id).collect();
                let pos = list_ids.iter().position(|x| *x == id);
                match key.code {
                    KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char(' ') => {
                        let step = if matches!(key.code, KeyCode::Down | KeyCode::Char('j')) { 1 } else { 10 };
                        self.detail_scroll = self.detail_scroll.saturating_add(step).min(self.layout.detail_max_scroll);
                    }
                    KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
                        let step = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) { 1 } else { 10 };
                        self.detail_scroll = self.detail_scroll.saturating_sub(step);
                    }
                    KeyCode::Tab | KeyCode::Right => {
                        self.reader_tab = self.reader_tab.next();
                        self.detail_scroll = 0;
                    }
                    KeyCode::BackTab | KeyCode::Left => {
                        self.reader_tab = self.reader_tab.prev();
                        self.detail_scroll = 0;
                    }
                    KeyCode::Char('y') => match scene.reports.iter().find(|r| r.id == id) {
                        Some(r) => {
                            let text = reader::tab_plain(r, self.reader_tab);
                            match ctl.copy_text(&text) {
                                Ok(()) => self.toast(
                                    format!("copied {} tab ({} chars)", self.reader_tab.label(), text.chars().count()),
                                    AlertLevel::Info,
                                    now_ms,
                                ),
                                Err(e) => self.toast(format!("copy failed: {e}"), AlertLevel::Critical, now_ms),
                            }
                        }
                        None => self.toast("report is gone", AlertLevel::Info, now_ms),
                    },
                    KeyCode::Char('w') => match scene.reports.iter().find(|r| r.id == id) {
                        Some(r) => {
                            let md = reader::report_markdown(r, scene.now_ms.max(now_ms));
                            match ctl.save_file(&format!("rosso-report-{id}.md"), &md) {
                                Ok(path) => self.toast(format!("saved {path}"), AlertLevel::Info, now_ms),
                                Err(e) => self.toast(format!("save failed: {e}"), AlertLevel::Critical, now_ms),
                            }
                        }
                        None => self.toast("report is gone", AlertLevel::Info, now_ms),
                    },
                    KeyCode::Char('n') | KeyCode::Char('p') => {
                        let next = match (pos, key.code) {
                            (Some(i), KeyCode::Char('n')) => list_ids.get(i + 1).copied(),
                            (Some(i), _) => i.checked_sub(1).and_then(|j| list_ids.get(j).copied()),
                            (None, _) => None,
                        };
                        match next {
                            Some(nid) => self.open_report(nid, only, inbox),
                            None => self.toast(if key.code == KeyCode::Char('n') { "last report" } else { "first report" }, AlertLevel::Info, now_ms),
                        }
                    }
                    KeyCode::Char('g') | KeyCode::Char('o') | KeyCode::Char('a') | KeyCode::Char('d') | KeyCode::Char('f') => {
                        let from = scene.reports.iter().find(|r| r.id == id).map(|r| r.from.clone());
                        match from.filter(|f| scene.agents.contains_key(f)) {
                            None => self.toast("sender is no longer in the office", AlertLevel::Info, now_ms),
                            Some(f) => match key.code {
                                KeyCode::Char('g') => {
                                    self.selected = Some(f);
                                    self.detail_scroll = 0;
                                    self.popup = Popup::None;
                                }
                                KeyCode::Char('f') => {
                                    self.detail_scroll = 0;
                                    self.popup = Popup::None;
                                    self.open_followup(scene, &f, now_ms);
                                }
                                code => {
                                    let prev = self.selected.replace(f);
                                    match code {
                                        KeyCode::Char('o') => self.resume(scene, ctl, now_ms),
                                        KeyCode::Char('a') => self.permission(scene, true, ctl, now_ms),
                                        _ => self.permission(scene, false, ctl, now_ms),
                                    }
                                    self.selected = prev;
                                }
                            },
                        }
                    }
                    KeyCode::Esc | KeyCode::Backspace | KeyCode::Enter => {
                        let sel = pos.unwrap_or(0);
                        self.detail_scroll = 0;
                        self.popup = Popup::Reports { sel, only, inbox };
                    }
                    KeyCode::Char('q') => {
                        self.detail_scroll = 0;
                        self.popup = Popup::None;
                    }
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::View { sel } => {
                let n = ShowConfig::ITEMS.len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Popup::View { sel: sel.saturating_sub(1) },
                    KeyCode::Down | KeyCode::Char('j') => self.popup = Popup::View { sel: (sel + 1).min(n - 1) },
                    KeyCode::Char(' ') | KeyCode::Enter => {
                        let v = !self.show.get(sel);
                        self.set_show_item(sel, v, view);
                    }
                    KeyCode::Char('a') => self.set_show_all(ShowConfig::all_on(), view),
                    KeyCode::Char('c') => {
                        let s = self.show.clean_on();
                        self.set_show_all(s, view);
                    }
                    KeyCode::Char('h') => {
                        let s = self.show.overlays_off();
                        self.set_show_all(s, view);
                    }
                    KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('q') => {
                        self.popup = Popup::None;
                        self.persist(now_ms);
                    }
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::Theme { sel, names, orig } => {
                let last = names.len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.theme_pick(sel.saturating_sub(1), names, orig, view, now_ms),
                    KeyCode::Down | KeyCode::Char('j') => self.theme_pick((sel + 1).min(last), names, orig, view, now_ms),
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i <= last {
                            self.theme_pick(i, names, orig, view, now_ms);
                        } else {
                            self.popup = Popup::Theme { sel, names, orig };
                        }
                    }
                    KeyCode::Enter => {
                        if sel >= last {
                            self.popup = Popup::Mixer { layer: 0 };
                        } else {
                            self.popup = Popup::None;
                            self.persist(now_ms);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') => self.close_theme_picker(orig, view, now_ms),
                    _ => self.popup = Popup::Theme { sel, names, orig },
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
            Popup::FreeRam { mut sel, targets, mut checked } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        sel = sel.saturating_sub(1);
                        self.popup = Popup::FreeRam { sel, targets, checked };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !targets.is_empty() {
                            sel = (sel + 1).min(targets.len() - 1);
                        }
                        self.popup = Popup::FreeRam { sel, targets, checked };
                    }
                    KeyCode::Char(' ') => {
                        if sel < checked.len() {
                            checked[sel] = !checked[sel];
                        }
                        self.popup = Popup::FreeRam { sel, targets, checked };
                    }
                    KeyCode::Char('a') => {
                        let all_on = checked.iter().any(|&c| !c);
                        checked.fill(all_on);
                        self.popup = Popup::FreeRam { sel, targets, checked };
                    }
                    KeyCode::Char('r') => {
                        self.popup = Popup::None;
                        self.reap_orphans_with_toast(ctl, now_ms);
                    }
                    KeyCode::Enter => {
                        self.popup = Popup::None;
                        let mut stopped = 0usize;
                        let mut freed_mb = 0u32;
                        let mut any_checked = false;
                        let mut last_err = None;
                        for (t, c) in targets.iter().zip(checked.iter()) {
                            if !*c {
                                continue;
                            }
                            any_checked = true;
                            if let Some(a) = scene.agents.get(&t.id) {
                                match ctl.kill_agent(a, t.pid) {
                                    Ok(()) => {
                                        stopped += 1;
                                        freed_mb = freed_mb.saturating_add(t.rss_mb);
                                    }
                                    Err(e) => last_err = Some(e),
                                }
                            }
                        }
                        if !any_checked {
                            self.toast("none selected", AlertLevel::Info, now_ms);
                        } else if stopped > 0 {
                            self.toast(format!("stopped {stopped} idle agents (~{freed_mb} MB)"), AlertLevel::Info, now_ms);
                        } else if let Some(e) = last_err {
                            self.toast(format!("stop failed: {e}"), AlertLevel::Critical, now_ms);
                        } else {
                            self.toast("none selected", AlertLevel::Info, now_ms);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('K') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::Dispatch { mut field, mut target_idx, mut cwd, mut prompt, followup } => {
                match key.code {
                    KeyCode::Esc => {
                        self.popup = Popup::None;
                    }
                    KeyCode::Tab => {
                        field = if followup.is_none() {
                            (field + 1) % 3
                        } else if field == 1 {
                            2
                        } else {
                            1
                        };
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::BackTab => {
                        field = if followup.is_none() {
                            (field + 2) % 3
                        } else if field == 1 {
                            2
                        } else {
                            1
                        };
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Up => {
                        field = if followup.is_none() { field.saturating_sub(1) } else { 1 };
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Down => {
                        field = (field + 1).min(2);
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Left if field == 0 && followup.is_none() => {
                        let n = DISPATCH_TARGETS.len();
                        target_idx = (target_idx + n - 1) % n;
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Right if field == 0 && followup.is_none() => {
                        target_idx = (target_idx + 1) % DISPATCH_TARGETS.len();
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Backspace => {
                        if field == 1 {
                            cwd.pop();
                        } else if field == 2 {
                            prompt.pop();
                        }
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        if field == 1 {
                            cwd.push(ch);
                        } else if field == 2 {
                            prompt.push(ch);
                        }
                        self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                    }
                    KeyCode::Enter => {
                        if prompt.trim().is_empty() {
                            self.popup = Popup::Dispatch { field, target_idx, cwd, prompt, followup };
                            self.toast("enter a prompt first", AlertLevel::Info, now_ms);
                        } else if let Some((aid, label)) = followup {
                            self.popup = Popup::None;
                            match scene.agents.get(&aid) {
                                Some(a) => match ctl.followup_session(a, &prompt) {
                                    Ok(msg) => self.toast(msg, AlertLevel::Info, now_ms),
                                    Err(e) => self.toast(format!("follow-up failed: {e}"), AlertLevel::Critical, now_ms),
                                },
                                None => self.toast(format!("{label} already left"), AlertLevel::Info, now_ms),
                            }
                        } else {
                            self.popup = Popup::None;
                            let target = DISPATCH_TARGETS[target_idx].0;
                            match ctl.dispatch_task(target, &cwd, &prompt) {
                                Ok(msg) => self.toast(msg, AlertLevel::Info, now_ms),
                                Err(e) => self.toast(format!("dispatch failed: {e}"), AlertLevel::Critical, now_ms),
                            }
                        }
                    }
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::Actions { mut sel } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        sel = sel.saturating_sub(1);
                        self.popup = Popup::Actions { sel };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        sel = (sel + 1).min(ACTIONS.len() - 1);
                        self.popup = Popup::Actions { sel };
                    }
                    KeyCode::Enter => {
                        let act = ACTIONS[sel].0;
                        self.popup = Popup::None;
                        self.run_action(act, scene, ctl, now_ms);
                    }
                    KeyCode::Char(c @ ('p' | 'f' | 'K' | 'R' | 'A' | 'i' | 'o' | 'B')) => {
                        self.popup = Popup::None;
                        let mut buf = [0u8; 4];
                        self.run_action(c.encode_utf8(&mut buf), scene, ctl, now_ms);
                    }
                    KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::ConfirmApproveAll { items } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.popup = Popup::None;
                        let mut approved = 0usize;
                        let mut last_err = None;
                        for (id, _label, req) in items {
                            if let Some(a) = scene.agents.get(&id) {
                                match ctl.resolve_permission(a, &req, true) {
                                    Ok(()) => approved += 1,
                                    Err(e) => last_err = Some(e),
                                }
                            }
                        }
                        if approved > 0 {
                            self.toast(format!("approved {approved} permissions"), AlertLevel::Info, now_ms);
                        } else if let Some(e) = last_err {
                            self.toast(format!("approve failed: {e}"), AlertLevel::Critical, now_ms);
                        } else {
                            self.toast("no pending permissions", AlertLevel::Info, now_ms);
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
            Popup::ConfirmAbortAuto { job, .. } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.popup = Popup::None;
                        match ctl.abort_auto(&job) {
                            Ok(msg) => self.toast(msg, AlertLevel::Info, now_ms),
                            Err(e) => self.toast(format!("abort failed: {e}"), AlertLevel::Critical, now_ms),
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.popup = Popup::None,
                    _ => {}
                }
                return Outcome::Continue;
            }
        }

        if let Some(r) = self.card_report(scene) {
            let id = r.id;
            match key.code {
                KeyCode::Enter => {
                    self.open_report(id, None, true);
                    return Outcome::Continue;
                }
                KeyCode::Esc => {
                    self.dismiss_card(scene);
                    return Outcome::Continue;
                }
                _ => {}
            }
        }

        if let KeyCode::Char(c) = key.code {
            if let Some(tab) = SideTab::from_digit(c) {
                self.side_tab = tab;
                self.sidebar = true;
                return Outcome::Continue;
            }
        }
        if self.sidebar && matches!(self.side_tab, SideTab::Jobs | SideTab::Activity) {
            let activity = self.side_tab == SideTab::Activity;
            let n = if activity { panels::feed(scene, &self.state_log, panels::FEED_MAX).len() } else { panels::jobs(scene).len() };
            let sel = if activity { &mut self.feed_sel } else { &mut self.jobs_sel };
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    *sel = (*sel).min(n.saturating_sub(1)).saturating_sub(1);
                    return Outcome::Continue;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *sel = (*sel + 1).min(n.saturating_sub(1));
                    return Outcome::Continue;
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    *sel = 0;
                    return Outcome::Continue;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    *sel = n.saturating_sub(1);
                    return Outcome::Continue;
                }
                KeyCode::Enter => {
                    let i = (*sel).min(n.saturating_sub(1));
                    let target = if activity {
                        panels::feed(scene, &self.state_log, panels::FEED_MAX).get(i).and_then(|f| f.report)
                    } else {
                        panels::jobs(scene).get(i).map(|j| j.last_report).filter(|&id| id > 0)
                    };
                    match target {
                        Some(id) => self.open_report(id, None, false),
                        None => self.toast(if n == 0 { "nothing here yet" } else { "no document for this line" }, AlertLevel::Info, now_ms),
                    }
                    return Outcome::Continue;
                }
                KeyCode::Char('R') if !activity => {
                    let rows = panels::jobs(scene);
                    if rows.is_empty() {
                        self.toast("nothing here yet", AlertLevel::Info, now_ms);
                    } else {
                        let i = self.jobs_sel.min(rows.len() - 1);
                        match &rows[i].auto {
                            Some(aj) => match ctl.resume_auto(&aj.job) {
                                Ok(msg) => self.toast(msg, AlertLevel::Info, now_ms),
                                Err(e) => self.toast(format!("resume failed: {e}"), AlertLevel::Critical, now_ms),
                            },
                            None => self.toast("selected job is not a rosso-auto job", AlertLevel::Info, now_ms),
                        }
                    }
                    return Outcome::Continue;
                }
                KeyCode::Char('X') if !activity => {
                    let rows = panels::jobs(scene);
                    if rows.is_empty() {
                        self.toast("nothing here yet", AlertLevel::Info, now_ms);
                    } else {
                        let i = self.jobs_sel.min(rows.len() - 1);
                        match &rows[i].auto {
                            Some(aj) => {
                                self.popup = Popup::ConfirmAbortAuto { job: aj.job.clone(), phase: aj.phase.clone() };
                            }
                            None => self.toast("selected job is not a rosso-auto job", AlertLevel::Info, now_ms),
                        }
                    }
                    return Outcome::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') => return Outcome::Quit,
            KeyCode::Char('i') => {
                if scene.reports.iter().any(|r| r.to.is_none()) {
                    self.popup = Popup::Reports { sel: 0, only: None, inbox: true };
                } else {
                    self.toast("inbox is empty", AlertLevel::Info, now_ms);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(scene, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(scene, 1),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(scene, i32::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(scene, i32::MAX / 2),
            KeyCode::Tab => self.jump_next_attention(scene, now_ms),
            KeyCode::Enter => {
                if let Some(a) = self.selected_agent(scene) {
                    self.detail_scroll = 0;
                    self.popup = Popup::Detail(a.id.clone());
                } else {
                    self.toast("no agent selected", AlertLevel::Info, now_ms);
                }
            }
            KeyCode::Char('t') => self.open_theme_picker(view),
            KeyCode::Char(']') => self.cycle_theme(1, view, now_ms),
            KeyCode::Char('[') => self.cycle_theme(-1, view, now_ms),
            KeyCode::Char('T') => self.popup = Popup::Mixer { layer: 0 },
            KeyCode::Char('v') => self.popup = Popup::View { sel: 0 },
            KeyCode::Char('z') => {
                let v = !self.show.fill_viewport;
                self.show.fill_viewport = v;
                view.set_show(&self.show);
                self.dirty = true;
                self.persist(now_ms);
                self.toast(if v { "scale: fill viewport" } else { "scale: pixel-exact" }, AlertLevel::Info, now_ms);
            }
            KeyCode::Char('L') => {
                let on = !(self.show.names || self.show.bubbles);
                self.set_show_item(0, on, view);
                self.set_show_item(1, on, view);
                self.persist(now_ms);
                self.toast(if on { "name tags + bubbles on" } else { "name tags + bubbles off" }, AlertLevel::Info, now_ms);
            }
            KeyCode::Char('s') => self.sidebar = !self.sidebar,
            KeyCode::Char('n') => {
                self.notifications = !self.notifications;
                self.persist(now_ms);
                let s = if self.notifications { "notifications on" } else { "notifications off" };
                self.toast(s, AlertLevel::Info, now_ms);
            }
            KeyCode::Char('m') => {
                self.sound = !self.sound;
                self.persist(now_ms);
                if self.sound {
                    self.chime(Chime::Test, now_ms, ctl);
                    self.toast("sound on (test chime played; chimes: needs you, inbox, errors)", AlertLevel::Info, now_ms);
                } else {
                    self.toast("sound off", AlertLevel::Info, now_ms);
                }
            }
            KeyCode::Char('c') => {
                let latest = scene.alerts.iter().map(|a| a.at_ms).max().unwrap_or(0);
                self.alerts_cleared_at = Some(latest.max(self.alerts_cleared_at.unwrap_or(0)));
                self.toast("alerts cleared (C to restore)", AlertLevel::Info, now_ms);
            }
            KeyCode::Char('C') => {
                self.alerts_cleared_at = None;
                self.toast("showing all alerts", AlertLevel::Info, now_ms);
            }
            KeyCode::Char('a') => self.permission(scene, true, ctl, now_ms),
            KeyCode::Char('d') => self.permission(scene, false, ctl, now_ms),
            KeyCode::Char('A') => self.open_approve_all(scene, now_ms),
            KeyCode::Char('x') => match self.selected_agent(scene) {
                Some(a) => match a.pid {
                    Some(pid) => self.popup = Popup::ConfirmCancel { id: a.id.clone(), label: a.label.clone(), pid },
                    None => self.toast(format!("{} has no live pid", a.label), AlertLevel::Info, now_ms),
                },
                None => self.toast("no agent selected", AlertLevel::Info, now_ms),
            },
            KeyCode::Char('K') => self.open_free_ram(scene, now_ms),
            KeyCode::Char('p') => {
                self.popup = Popup::Dispatch {
                    field: 2,
                    target_idx: 0,
                    cwd: self.default_cwd(scene),
                    prompt: String::new(),
                    followup: None,
                };
            }
            KeyCode::Char('f') => match self.selected_agent(scene).map(|a| a.id.clone()) {
                Some(id) => self.open_followup(scene, &id, now_ms),
                None => self.toast("no agent selected", AlertLevel::Info, now_ms),
            },
            KeyCode::Char(' ') => self.popup = Popup::Actions { sel: 0 },
            KeyCode::Char('o') => self.resume(scene, ctl, now_ms),
            KeyCode::Char('?') => {
                self.detail_scroll = 0;
                self.popup = Popup::Help;
            }
            KeyCode::Char('r') => {
                let only = self.selected.clone().filter(|id| scene.reports.iter().any(|r| &r.from == id || r.to.as_ref() == Some(id)));
                if scene.reports.is_empty() {
                    self.toast("no hand-offs yet", AlertLevel::Info, now_ms);
                } else {
                    self.popup = Popup::Reports { sel: 0, only, inbox: false };
                }
            }
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
        let t = match a.kind {
            AgentKind::Claude | AgentKind::Agy | AgentKind::Other => a,
            AgentKind::ClaudeSub => a.parent.as_ref().and_then(|p| scene.agents.get(p)).unwrap_or(a),
        };
        let Some(rid) = t.resume_id.clone().filter(|s| !s.is_empty()) else {
            let l = t.label.clone();
            self.toast(format!("{l}: no session id to resume"), AlertLevel::Info, now_ms);
            return;
        };
        let cwd = t.cwd.clone();
        match ctl.resume_agent(t, cwd.as_deref(), &rid) {
            Ok(cmd) => self.toast(format!("copied: {cmd}"), AlertLevel::Info, now_ms),
            Err(e) => self.toast(format!("resume failed: {e}"), AlertLevel::Critical, now_ms),
        }
    }

    /// Mouse: left click on a sidebar row, alert row, or office agent selects it; double-click opens details.
    /// `hit` maps office pixel coordinates to an agent (Renderer::hit_test).
    #[cfg(test)]
    pub fn handle_mouse(&mut self, ev: MouseEvent, scene: &Scene, hit: &dyn Fn(u32, u32) -> Option<AgentId>) -> bool {
        self.handle_mouse_at(ev, scene, hit, office_core::paths::now_ms())
    }

    pub fn handle_mouse_at(
        &mut self,
        ev: MouseEvent,
        scene: &Scene,
        hit: &dyn Fn(u32, u32) -> Option<AgentId>,
        now_ms: u64,
    ) -> bool {
        if self.popup != Popup::None {
            return match ev.kind {
                MouseEventKind::ScrollUp if matches!(self.popup, Popup::Detail(_) | Popup::ReportView { .. }) => {
                    self.detail_scroll = self.detail_scroll.saturating_sub(2);
                    true
                }
                MouseEventKind::ScrollDown if matches!(self.popup, Popup::Detail(_) | Popup::ReportView { .. }) => {
                    self.detail_scroll = self.detail_scroll.saturating_add(2).min(self.layout.detail_max_scroll);
                    true
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let p = self.layout.popup;
                    let inside = p.width > 0
                        && ev.column >= p.x
                        && ev.column < p.x + p.width
                        && ev.row >= p.y
                        && ev.row < p.y + p.height;
                    if !inside {
                        let was_mixer = matches!(self.popup, Popup::Mixer { .. } | Popup::View { .. });
                        if let Popup::Theme { orig, .. } = self.popup.clone() {
                            self.mix = orig.0.clone();
                            self.pending_mix = Some(orig.0);
                        }
                        self.popup = Popup::None;
                        self.detail_scroll = 0;
                        if was_mixer {
                            self.persist(now_ms);
                        }
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
        }
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
                MouseEventKind::ScrollUp => {
                    self.move_selection(scene, -1);
                    true
                }
                MouseEventKind::ScrollDown => {
                    self.move_selection(scene, 1);
                    true
                }
                _ => false,
            };
        }
        let (x, y) = (ev.column, ev.row);
        let btn = self.layout.free_ram_btn;
        if btn.width > 0 && x >= btn.x && x < btn.x + btn.width && y >= btn.y && y < btn.y + btn.height {
            self.open_free_ram(scene, now_ms);
            self.dirty = true;
            return true;
        }
        let tb = self.layout.side_tabs;
        if self.sidebar && tb.width > 0 && x >= tb.x && x < tb.x + tb.width && y >= tb.y && y < tb.y + tb.height {
            if let Some(t) = SideTab::at_offset(x - tb.x) {
                self.side_tab = t;
                self.dirty = true;
                return true;
            }
            return false;
        }
        let r = self.layout.agent_rows;
        if self.sidebar && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
            if let Some(id) = self.layout.row_ids.get((y - r.y) as usize).cloned() {
                let dbl = self
                    .last_click
                    .as_ref()
                    .is_some_and(|(t, prev)| prev == &id && now_ms.saturating_sub(*t) <= 400);
                self.selected = Some(id.clone());
                self.last_click = Some((now_ms, id.clone()));
                if dbl && scene.agents.contains_key(&id) {
                    self.detail_scroll = 0;
                    self.popup = Popup::Detail(id);
                }
                self.dirty = true;
                return true;
            }
            return false;
        }
        let ar = self.layout.alert_rows;
        if self.sidebar && x >= ar.x && x < ar.x + ar.width && y >= ar.y && y < ar.y + ar.height {
            if let Some(Some(id)) = self.layout.alert_agent_ids.get((y - ar.y) as usize).cloned() {
                if scene.agents.contains_key(&id) {
                    self.selected = Some(id);
                    self.dirty = true;
                    return true;
                }
            }
            return false;
        }
        if let Some(id) = self.office_hit(x, y, hit) {
            if scene.agents.contains_key(&id) || is_ambient(&id) {
                let dbl = !is_ambient(&id)
                    && self
                        .last_click
                        .as_ref()
                        .is_some_and(|(t, prev)| prev == &id && now_ms.saturating_sub(*t) <= 400);
                self.selected = Some(id.clone());
                self.last_click = Some((now_ms, id.clone()));
                if dbl {
                    self.detail_scroll = 0;
                    self.popup = Popup::Detail(id);
                }
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
            h.press(key(']'));
            seen.push(h.app.mix.floor.clone());
            assert!(mix_eq(&h.view.mix, &h.app.mix));
        }
        assert_eq!(seen, vec!["modern", "cozy", "retro", "cyberpunk", "procedural"]);
        // Missing themes are skipped.
        let mut h = Harness::new();
        h.view = FakeView::new(&["procedural", "retro"]);
        h.press(key(']'));
        assert_eq!(h.app.mix.characters, "retro");
        h.press(key(']'));
        assert_eq!(h.app.mix.characters, "procedural");
    }

    #[test]
    fn theme_picker_previews_restores_and_persists() {
        let mut h = Harness::new();
        h.press(key('t'));
        match &h.app.popup {
            Popup::Theme { sel, names, .. } => {
                assert_eq!(*sel, 0);
                assert_eq!(names[0], "procedural");
                assert_eq!(names.len(), 5);
            }
            p => panic!("{p:?}"),
        }
        h.press(key('j'));
        h.press(key('j'));
        assert_eq!(h.view.mix.floor, "cozy");
        assert!(mix_eq(&h.view.mix, &h.app.mix));
        h.press(key('4'));
        assert_eq!(h.view.mix.floor, "retro");
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.view.mix.floor, "procedural");
        assert_eq!(h.app.mix.floor, "procedural");
        // Enter keeps and persists.
        let dir = std::env::temp_dir().join(format!("office-pick-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        h.app.config_path = Some(path.clone());
        h.press(key('t'));
        h.press(key('j'));
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.app.mix.floor, "modern");
        let cfg: office_core::config::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.theme.floor, "modern");
        let _ = std::fs::remove_dir_all(&dir);
        // Reopening selects the current theme; custom row opens the mixer; `[` cycles back.
        h.press(key('t'));
        assert!(matches!(h.app.popup, Popup::Theme { sel: 1, .. }));
        for _ in 0..4 {
            h.press(key('j'));
        }
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::Mixer { layer: 0 });
        h.press(code(KeyCode::Esc));
        h.press(key('[')); // the custom row kept the last previewed preset (cyberpunk)
        assert_eq!(h.app.mix.floor, "retro");
    }

    #[test]
    fn theme_is_persisted_preserving_other_keys() {
        let dir = std::env::temp_dir().join(format!("office-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "fps = 30\n[theme]\nfloor = \"x\"\n").unwrap();
        let mut h = Harness::new();
        h.app.config_path = Some(path.clone());
        h.press(key(']'));
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
        assert!(h.toast().contains("no live pid"));
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
        assert!(h.toast().contains("no session id to resume"));
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

    #[test]
    fn tab_jumps_needs_you_then_error_then_active() {
        let mut h = Harness::new();
        h.scene.agents.get_mut("agy:4242").unwrap().state = AgentState::Error;
        // demo_scene has cc:office in NeedsYou -> Tab jumps there first.
        select(&mut h.app, "cc:dice");
        h.press(code(KeyCode::Tab));
        assert_eq!(h.app.selected.as_deref(), Some("cc:office"));
        // Remove NeedsYou -> Tab jumps to Error (agy:4242).
        h.scene.agents.get_mut("cc:office").unwrap().state = AgentState::Idle;
        h.press(code(KeyCode::Tab));
        assert_eq!(h.app.selected.as_deref(), Some("agy:4242"));
        // Clear Error too -> Tab jumps to active agent (cc:dice Reading or Explore Thinking).
        h.scene.agents.get_mut("agy:4242").unwrap().state = AgentState::Idle;
        h.press(code(KeyCode::Tab));
        assert_eq!(h.app.selected.as_deref(), Some("cc:dice"));
        h.press(code(KeyCode::Tab));
        assert_eq!(h.app.selected.as_deref(), Some("cc-sub:dice/1"));
        // When all agents are Idle -> informative toast.
        for a in h.scene.agents.values_mut() {
            a.state = AgentState::Idle;
        }
        h.press(code(KeyCode::Tab));
        assert!(h.toast().contains("no active"), "{}", h.toast());
    }

    #[test]
    fn sound_toggle_and_chime_triggers() {
        use super::super::chime::Chime;
        let mut h = Harness::new();
        assert!(!h.app.sound);
        // Sound off: no chime even for a needs-you transition.
        h.scene.agents.get_mut("cc:dice").unwrap().state = AgentState::NeedsYou;
        h.app.observe(&h.scene, 1_000, &mut h.ctl);
        assert!(h.ctl.sounds.is_empty());
        h.scene.agents.get_mut("cc:dice").unwrap().state = AgentState::Idle;
        h.app.observe(&h.scene, 2_000, &mut h.ctl);
        h.press(key('m'));
        assert!(h.app.sound);
        assert!(h.toast().starts_with("sound on"), "{}", h.toast());
        assert_eq!(h.ctl.sounds, vec![Chime::Test]);
        // Notifications off must not silence chimes.
        h.app.notifications = false;
        h.scene.agents.get_mut("cc:dice").unwrap().state = AgentState::NeedsYou;
        h.app.observe(&h.scene, 20_000, &mut h.ctl);
        // A critical alert 1 s later is rate limited; one 4 s later plays the error chime.
        h.scene.alerts.push(Alert {
            at_ms: 21_000,
            level: AlertLevel::Critical,
            agent: Some("agy:4242".into()),
            code: "timeout".into(),
            message: "timed out".into(),
        });
        h.app.observe(&h.scene, 21_000, &mut h.ctl);
        h.scene.alerts.push(Alert {
            at_ms: 24_000,
            level: AlertLevel::Critical,
            agent: Some("agy:4242".into()),
            code: "oops".into(),
            message: "again".into(),
        });
        h.app.observe(&h.scene, 24_000, &mut h.ctl);
        assert_eq!(h.ctl.sounds, vec![Chime::Test, Chime::NeedsYou, Chime::Error]);
        h.press(key('m'));
        assert!(!h.app.sound);
        assert_eq!(h.toast(), "sound off");
    }

    #[test]
    fn sound_and_notification_settings_persist() {
        let path = std::env::temp_dir().join(format!("office-sound-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut h = Harness::new();
        h.app.config_path = Some(path.clone());
        h.press(key('m'));
        let t: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(t["sound"].as_bool(), Some(true));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn alerts_clear_restore_and_detail_scroll() {
        let mut h = Harness::new();
        assert_eq!(h.app.visible_alerts(&h.scene).len(), 2);
        h.press(key('c'));
        assert_eq!(h.app.visible_alerts(&h.scene).len(), 0);
        assert!(h.toast().contains("C to restore"));
        h.press(key('C'));
        assert_eq!(h.app.visible_alerts(&h.scene).len(), 2);
        assert_eq!(h.toast(), "showing all alerts");

        select(&mut h.app, "cc:office");
        h.press(code(KeyCode::Enter));
        h.app.layout.detail_max_scroll = 100;
        assert_eq!(h.app.detail_scroll, 0);
        h.press(code(KeyCode::PageDown));
        assert_eq!(h.app.detail_scroll, 6);
        h.press(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(h.app.detail_scroll, 12);
        h.press(code(KeyCode::PageUp));
        assert_eq!(h.app.detail_scroll, 6);
        h.press(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(h.app.detail_scroll, 0);
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
    }

    #[test]
    fn mouse_double_click_alerts_and_modal_outside_close() {
        let mut h = Harness::new();
        h.app.layout.agent_rows = Rect::new(60, 3, 40, 5);
        h.app.layout.row_ids = vec!["cc:dice".into(), "agy:4242".into()];
        h.app.layout.alert_rows = Rect::new(60, 20, 40, 4);
        h.app.layout.alert_agent_ids = vec![Some("cc-sub:dice/1".into()), None];
        h.app.layout.office = Rect::new(0, 1, 58, 20);
        h.app.layout.font_px = Some((10, 20));
        let click = |c, r| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };
        // Clicking alert row 0 selects its linked agent.
        assert!(h.app.handle_mouse_at(click(62, 20), &h.scene, &|_, _| None, 100));
        assert_eq!(h.app.selected.as_deref(), Some("cc-sub:dice/1"));
        // Double-click agent row 1 within 400ms opens Detail popup.
        assert!(h.app.handle_mouse_at(click(65, 4), &h.scene, &|_, _| None, 200));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.app.handle_mouse_at(click(65, 4), &h.scene, &|_, _| None, 350));
        assert_eq!(h.app.popup, Popup::Detail("agy:4242".into()));

        // While modal is open, hover behind modal does nothing; scroll changes detail_scroll; click outside closes.
        h.app.layout.popup = Rect::new(20, 5, 50, 15);
        h.app.layout.detail_max_scroll = 100;
        let mv = MouseEvent { kind: MouseEventKind::Moved, column: 3, row: 3, modifiers: KeyModifiers::NONE };
        assert!(!h.app.handle_mouse_at(mv, &h.scene, &|_, _| Some("cc:office".into()), 400));
        assert_eq!(h.app.hover, None);
        let scr_dn = MouseEvent { kind: MouseEventKind::ScrollDown, column: 30, row: 10, modifiers: KeyModifiers::NONE };
        assert!(h.app.handle_mouse_at(scr_dn, &h.scene, &|_, _| None, 410));
        assert_eq!(h.app.detail_scroll, 2);
        // Click inside popup keeps it open.
        assert!(!h.app.handle_mouse_at(click(25, 8), &h.scene, &|_, _| None, 420));
        assert!(matches!(h.app.popup, Popup::Detail(_)));
        // Click outside popup closes it and resets scroll.
        assert!(h.app.handle_mouse_at(click(2, 2), &h.scene, &|_, _| None, 430));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.app.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_clamps_to_max_and_mixer_persists_on_click_outside() {
        let mut h = Harness::new();
        h.app.popup = Popup::Detail("cc:office".into());
        h.app.layout.detail_max_scroll = 3;
        let scr_dn = MouseEvent { kind: MouseEventKind::ScrollDown, column: 30, row: 10, modifiers: KeyModifiers::NONE };
        h.app.handle_mouse_at(scr_dn, &h.scene, &|_, _| None, 100);
        h.app.handle_mouse_at(scr_dn, &h.scene, &|_, _| None, 110);
        assert_eq!(h.app.detail_scroll, 3);

        h.app.popup = Popup::Mixer { layer: 0 };
        h.app.layout.popup = Rect::new(20, 5, 50, 10);
        let click_out = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 1, row: 1, modifiers: KeyModifiers::NONE };
        assert!(h.app.handle_mouse_at(click_out, &h.scene, &|_, _| None, 200));
        assert_eq!(h.app.popup, Popup::None);
    }

    #[test]
    fn r_opens_hand_offs_for_the_selected_agent_and_reads_one() {
        let mut h = Harness::new();
        h.scene.reports.push(office_core::Report {
            id: 7, at_ms: 1, kind: office_core::ReportKind::Report, from: "cc-sub:dice/1".into(), from_label: "Explore".into(),
            to: Some("cc:dice".into()), to_label: "cc·DICE".into(), title: "Findings".into(), body: "all good".into(), brief: String::new(),
        });
        h.scene.reports.push(office_core::Report {
            id: 8, at_ms: 2, kind: office_core::ReportKind::Result, from: "cc:office".into(), from_label: "cc·office".into(),
            to: None, to_label: "you".into(), title: "Turn done".into(), body: String::new(), brief: String::new(),
        });
        select(&mut h.app, "cc-sub:dice/1");
        h.press(key('r'));
        assert_eq!(h.app.popup, Popup::Reports { sel: 0, only: Some("cc-sub:dice/1".into()), inbox: false });
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::ReportView { id: 7, only: Some("cc-sub:dice/1".into()), inbox: false });
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::Reports { sel: 0, only: Some("cc-sub:dice/1".into()), inbox: false });
        h.press(key('a'));
        assert_eq!(h.app.popup, Popup::Reports { sel: 0, only: None, inbox: false });
        assert_eq!(report_list(&h.scene, None).len(), 2);
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
    }

    fn boss_report(id: u64, from: &str, title: &str) -> office_core::Report {
        office_core::Report {
            id, at_ms: id, kind: office_core::ReportKind::Result, from: from.into(), from_label: format!("label-{from}"),
            to: None, to_label: "you".into(), title: title.into(), body: "line one\n\nline two\nline three".into(), brief: String::new(),
        }
    }

    #[test]
    fn inbox_startup_reports_are_seen_and_new_ones_get_a_card() {
        let cfg = office_core::config::Config::default();
        let mut app = App::new(&cfg, false);
        app.config_path = None;
        let mut scene = demo_scene();
        scene.reports.push(boss_report(3, "cc:dice", "old"));
        let mut ctl = FakeControl::default();
        app.observe(&scene, 1_000, &mut ctl);
        assert_eq!(app.unread_count(&scene), 0);
        assert!(app.card_report(&scene).is_none());
        let base = ctl.notes.len();
        scene.reports.push(boss_report(4, "cc:dice", "fresh"));
        app.observe(&scene, 2_000, &mut ctl);
        assert_eq!(app.unread_count(&scene), 1);
        assert_eq!(app.card_report(&scene).unwrap().id, 4);
        assert_eq!(ctl.notes.len(), base + 1);
        app.observe(&scene, 30_000, &mut ctl);
        assert_eq!(ctl.notes.len(), base + 1, "one notification per report");
    }

    #[test]
    fn inbox_card_enter_reads_and_esc_dismisses() {
        let mut h = Harness::new();
        h.scene.reports.push(boss_report(9, "cc:dice", "done"));
        h.app.observe(&h.scene, 2_000, &mut h.ctl);
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::ReportView { id: 9, only: None, inbox: true });
        assert_eq!(h.app.unread_count(&h.scene), 0);
        h.press(code(KeyCode::Esc));
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);

        h.scene.reports.push(boss_report(10, "cc:dice", "again"));
        h.app.observe(&h.scene, 3_000, &mut h.ctl);
        assert!(h.app.card_report(&h.scene).is_some());
        h.press(code(KeyCode::Esc));
        assert!(h.app.card_report(&h.scene).is_none());
        assert_eq!(h.app.popup, Popup::None);
        // Enter now opens the agent detail again.
        h.press(code(KeyCode::Enter));
        assert!(matches!(h.app.popup, Popup::Detail(_)));
    }

    #[test]
    fn inbox_key_lists_only_boss_reports_and_reader_navigates() {
        let mut h = Harness::new();
        h.press(key('i'));
        assert_eq!(h.toast(), "inbox is empty");
        h.scene.reports.push(boss_report(1, "cc:dice", "first"));
        let mut peer = boss_report(2, "cc-sub:dice/1", "peer");
        peer.to = Some("cc:dice".into());
        h.scene.reports.push(peer);
        h.scene.reports.push(boss_report(3, "cc:dice", "third"));
        h.press(key('i'));
        assert_eq!(h.app.popup, Popup::Reports { sel: 0, only: None, inbox: true });
        assert_eq!(report_list_for(&h.scene, None, true).iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 1]);
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::ReportView { id: 3, only: None, inbox: true });
        h.press(key('n'));
        assert_eq!(h.app.popup, Popup::ReportView { id: 1, only: None, inbox: true });
        h.press(key('p'));
        assert_eq!(h.app.popup, Popup::ReportView { id: 3, only: None, inbox: true });
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::Reports { sel: 0, only: None, inbox: true });
    }

    #[test]
    fn reader_g_selects_sender_and_o_resumes_it() {
        let mut h = Harness::new();
        h.scene.reports.push(boss_report(5, "cc:dice", "done"));
        select(&mut h.app, "cc:office");
        h.app.popup = Popup::ReportView { id: 5, only: None, inbox: true };
        h.press(key('o'));
        assert!(h.ctl.calls.iter().any(|c| c == "resume /tmp/dice sess-dice"), "{:?}", h.ctl.calls);
        assert_eq!(h.app.selected.as_deref(), Some("cc:office"), "o must not change the selection");
        assert!(matches!(h.app.popup, Popup::ReportView { .. }));
        h.press(key('g'));
        assert_eq!(h.app.selected.as_deref(), Some("cc:dice"));
        assert_eq!(h.app.popup, Popup::None);
        // a sender that left is reported, not crashed on
        h.scene.reports.push(boss_report(6, "gone:1", "bye"));
        h.app.popup = Popup::ReportView { id: 6, only: None, inbox: true };
        h.press(key('g'));
        assert_eq!(h.toast(), "sender is no longer in the office");
    }

    #[test]
    fn view_popup_navigates_toggles_and_persists() {
        let dir = std::env::temp_dir().join(format!("office-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let mut h = Harness::new();
        h.app.config_path = Some(path.clone());
        h.press(key('v'));
        assert_eq!(h.app.popup, Popup::View { sel: 0 });
        h.press(key('j'));
        h.press(code(KeyCode::Down));
        h.press(key('k'));
        assert_eq!(h.app.popup, Popup::View { sel: 1 });
        h.press(key(' '));
        assert!(!h.app.show.bubbles && h.app.show.names);
        assert!(!h.view.show.bubbles, "applied live");
        h.press(code(KeyCode::Enter));
        assert!(h.app.show.bubbles);
        h.press(key('h'));
        let s = &h.view.show;
        assert!(!s.names && !s.bubbles && !s.icons && !s.nameplates && !s.couriers);
        assert!(s.zones && s.cat && s.ceo);
        h.press(key('a'));
        assert_eq!(h.view.show, ShowConfig::default());
        h.press(key('h'));
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
        let cfg: office_core::config::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!cfg.show.names && cfg.show.zones);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capital_l_toggles_labels() {
        let mut h = Harness::new();
        h.press(key('L'));
        assert!(!h.app.show.names && !h.app.show.bubbles);
        assert!(!h.view.show.names && !h.view.show.bubbles);
        assert_eq!(h.toast(), "name tags + bubbles off");
        h.press(key('L'));
        assert!(h.app.show.names && h.app.show.bubbles);
        assert_eq!(h.toast(), "name tags + bubbles on");
    }

    fn reader_report(id: u64) -> office_core::Report {
        office_core::Report {
            id, at_ms: 1, kind: office_core::ReportKind::Report, from: "cc:dice".into(), from_label: "cc·DICE".into(),
            to: None, to_label: "you".into(), title: "Review: plan [PASS]".into(),
            body: "STATUS: OK\nRESPONSE:\nAll good.\n\n## Findings\n- one".into(), brief: "check it".into(),
        }
    }

    #[test]
    fn reader_tabs_switch_with_tab_and_arrows() {
        let mut h = Harness::new();
        h.scene.reports.push(reader_report(5));
        h.app.popup = Popup::ReportView { id: 5, only: None, inbox: false };
        assert_eq!(h.app.reader_tab, ReaderTab::Summary);
        h.press(code(KeyCode::Tab));
        assert_eq!(h.app.reader_tab, ReaderTab::Full);
        h.press(code(KeyCode::Right));
        assert_eq!(h.app.reader_tab, ReaderTab::Brief);
        h.press(code(KeyCode::Right));
        assert_eq!(h.app.reader_tab, ReaderTab::Raw);
        h.press(code(KeyCode::Right));
        assert_eq!(h.app.reader_tab, ReaderTab::Summary);
        h.press(code(KeyCode::Left));
        assert_eq!(h.app.reader_tab, ReaderTab::Raw);
        h.press(code(KeyCode::BackTab));
        assert_eq!(h.app.reader_tab, ReaderTab::Brief);
        assert!(matches!(h.app.popup, Popup::ReportView { id: 5, .. }), "tabs keep the popup open");
    }

    #[test]
    fn reader_y_copies_the_current_tab_and_w_saves_markdown() {
        let mut h = Harness::new();
        h.scene.reports.push(reader_report(5));
        h.app.popup = Popup::ReportView { id: 5, only: None, inbox: false };
        h.app.reader_tab = ReaderTab::Brief;
        h.press(key('y'));
        assert_eq!(h.ctl.clipboard, vec!["check it".to_string()]);
        assert!(h.toast().starts_with("copied Brief tab"), "{}", h.toast());
        h.app.reader_tab = ReaderTab::Raw;
        h.press(key('y'));
        assert_eq!(h.ctl.clipboard[1], h.scene.reports[0].body);
        h.press(key('w'));
        assert_eq!(h.ctl.saved.len(), 1);
        assert_eq!(h.ctl.saved[0].0, "rosso-report-5.md");
        assert!(h.ctl.saved[0].1.starts_with("# Review: plan [PASS]") && h.ctl.saved[0].1.contains("All good."));
        assert_eq!(h.toast(), "saved ~/Downloads/rosso-report-5.md");
        h.ctl.fail = true;
        h.press(key('y'));
        assert!(h.toast().starts_with("copy failed"), "{}", h.toast());
        h.press(key('w'));
        assert!(h.toast().starts_with("save failed"), "{}", h.toast());
    }

    #[test]
    fn digits_switch_sidebar_tabs_but_not_inside_popups() {
        let mut h = Harness::new();
        assert_eq!(h.app.side_tab, SideTab::Agents);
        h.press(key('2'));
        assert_eq!(h.app.side_tab, SideTab::Jobs);
        h.press(key('3'));
        assert_eq!(h.app.side_tab, SideTab::Activity);
        h.press(key('4'));
        assert_eq!(h.app.side_tab, SideTab::Stats);
        h.press(key('1'));
        assert_eq!(h.app.side_tab, SideTab::Agents);
        h.press(key('5'));
        assert_eq!(h.app.side_tab, SideTab::Agents);
        // The theme picker keeps its own 1-9.
        h.press(key('t'));
        h.press(key('3'));
        assert!(matches!(h.app.popup, Popup::Theme { sel: 2, .. }), "{:?}", h.app.popup);
        assert_eq!(h.app.side_tab, SideTab::Agents);
        h.press(code(KeyCode::Esc));
        // Hidden sidebar comes back when you pick a tab.
        h.app.sidebar = false;
        h.press(key('4'));
        assert!(h.app.sidebar && h.app.side_tab == SideTab::Stats);
    }

    #[test]
    fn jobs_and_activity_lists_navigate_and_open_reports() {
        let mut h = Harness::new();
        let mk = |id: u64, at: u64, title: &str| office_core::Report {
            id, at_ms: at, kind: office_core::ReportKind::Report, from: "rosso:A".into(), from_label: "A".into(),
            to: Some("rosso:B".into()), to_label: "B".into(), title: title.into(), body: "x".into(), brief: String::new(),
        };
        h.scene.reports.push(mk(1, 10, "Memo: one"));
        h.scene.reports.push(mk(2, 20, "Memo: two"));
        h.scene.report_jobs.insert(1, "job-a".into());
        h.scene.report_jobs.insert(2, "job-b".into());
        h.press(key('2'));
        h.press(key('j'));
        assert_eq!(h.app.jobs_sel, 1);
        h.press(key('j'));
        assert_eq!(h.app.jobs_sel, 1, "clamped");
        h.press(code(KeyCode::Enter));
        // Newest job first: row 1 is job-a (report 1).
        assert!(matches!(h.app.popup, Popup::ReportView { id: 1, .. }), "{:?}", h.app.popup);
        h.press(code(KeyCode::Esc));
        h.press(code(KeyCode::Esc));
        h.press(key('3'));
        h.press(code(KeyCode::Enter));
        assert!(matches!(h.app.popup, Popup::ReportView { id: 2, .. }), "newest hand-off first: {:?}", h.app.popup);
        h.press(code(KeyCode::Esc));
        h.press(code(KeyCode::Esc));
        // On the agents tab, j/k still move the agent selection.
        h.press(key('1'));
        let before = h.app.selected.clone();
        h.press(key('j'));
        assert_ne!(h.app.selected, before);
    }

    #[test]
    fn activity_log_records_notable_state_changes_and_samples_are_bounded() {
        let mut h = Harness::new();
        h.scene.agents.get_mut("cc:dice").unwrap().state = AgentState::NeedsYou;
        h.app.observe(&h.scene, 7_000, &mut h.ctl);
        assert_eq!(h.app.state_log.len(), 1);
        assert_eq!(h.app.state_log[0].label, "cc·DICE");
        h.app.observe(&h.scene, 8_000, &mut h.ctl);
        assert_eq!(h.app.state_log.len(), 1, "no repeat without a change");
        for i in 0..(panels::SAMPLE_CAP as u64 + 40) {
            h.app.tick(&h.scene, 100_000 + i * 10_000);
        }
        assert_eq!(h.app.samples.cost.len(), panels::SAMPLE_CAP);
    }

    #[test]
    fn clicking_the_tab_bar_switches_tabs() {
        let mut h = Harness::new();
        h.app.layout.side_tabs = Rect::new(80, 1, 40, 1);
        let click = |x| MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: 1, modifiers: KeyModifiers::NONE };
        assert!(h.app.handle_mouse_at(click(80 + 10), &h.scene, &|_, _| None, 100));
        assert_eq!(h.app.side_tab, SideTab::Jobs);
        assert!(h.app.handle_mouse_at(click(80 + 28), &h.scene, &|_, _| None, 100));
        assert_eq!(h.app.side_tab, SideTab::Stats);
    }

    #[test]
    fn idle_kill_candidates_filters_and_sorts_by_rss() {
        let mut s = Scene::default();
        let mk = |id: &str, kind: AgentKind, state: AgentState, pid: Option<u32>, rss: Option<u32>, parent: Option<&str>| {
            let mut a = agent(id, kind, parent);
            a.state = state;
            a.pid = pid;
            a.rss_mb = rss;
            a.last_event_ms = 1_000;
            a
        };
        // Active & NeedsYou excluded.
        s.agents.insert("active".into(), mk("active", AgentKind::Claude, AgentState::Running, Some(100), Some(900), None));
        s.agents.insert("needs".into(), mk("needs", AgentKind::Claude, AgentState::NeedsYou, Some(101), Some(800), None));
        // Parent with an active child excluded.
        s.agents.insert("parent-busy".into(), mk("parent-busy", AgentKind::Claude, AgentState::Idle, Some(102), Some(700), None));
        s.agents.insert("child-busy".into(), mk("child-busy", AgentKind::Agy, AgentState::Typing, Some(103), Some(200), Some("parent-busy")));
        // Parent whose child is Done IS eligible.
        s.agents.insert("parent-ok".into(), mk("parent-ok", AgentKind::Claude, AgentState::Idle, Some(104), Some(300), None));
        s.agents.insert("child-done".into(), mk("child-done", AgentKind::Agy, AgentState::Done, Some(105), Some(150), Some("parent-ok")));
        // Orphan with pid > 1 and Done state included; pid <= 1 / None excluded.
        s.agents.insert("ag-orphan:200".into(), mk("ag-orphan:200", AgentKind::Other, AgentState::Idle, Some(200), Some(500), None));
        s.agents.insert("no-pid".into(), mk("no-pid", AgentKind::Claude, AgentState::Idle, None, Some(999), None));
        s.agents.insert("pid-1".into(), mk("pid-1", AgentKind::Claude, AgentState::Idle, Some(1), Some(999), None));

        let cands = idle_kill_candidates(&s, 10_000);
        let ids: Vec<&str> = cands.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["ag-orphan:200", "parent-ok", "child-done"]);
        assert!(cands[0].is_orphan);
        assert!(!cands[1].is_orphan);
        assert_eq!(cands[0].rss_mb, 500);
        assert_eq!(cands[0].idle_ms, 9_000);
    }

    #[test]
    fn free_ram_popup_toggles_kills_and_opens_on_button_click() {
        let mut h = Harness::new();
        // Empty candidates + 0 orphans -> toast, stays None.
        h.press(key('K'));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.toast().contains("no idle killable"));

        // Add two idle candidates with PIDs.
        let mut a1 = agent("cc:idle1", AgentKind::Claude, None);
        a1.pid = Some(1111);
        a1.rss_mb = Some(400);
        a1.state = AgentState::Idle;
        let mut a2 = agent("ag-orphan:2222", AgentKind::Other, None);
        a2.pid = Some(2222);
        a2.rss_mb = Some(250);
        a2.state = AgentState::Done;
        h.scene.agents.insert(a1.id.clone(), a1);
        h.scene.agents.insert(a2.id.clone(), a2);

        h.press(key('K'));
        assert!(matches!(h.app.popup, Popup::FreeRam { sel: 0, .. }));
        // Space unchecks first target, Enter kills only second target.
        h.press(key(' '));
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.killed, vec![2222]);
        assert!(h.toast().contains("stopped 1 idle agents (~250 MB)"), "{}", h.toast());

        // Clicking free_ram_btn opens FreeRam.
        h.app.layout.free_ram_btn = Rect::new(50, 0, 12, 1);
        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 55, row: 0, modifiers: KeyModifiers::NONE };
        assert!(h.app.handle_mouse_at(click, &h.scene, &|_, _| None, 5_000));
        assert!(matches!(h.app.popup, Popup::FreeRam { .. }));
    }

    #[test]
    fn dispatch_popup_switches_targets_types_and_submits() {
        let mut h = Harness::new();
        select(&mut h.app, "cc:dice");
        h.press(key('p'));
        assert!(matches!(
            &h.app.popup,
            Popup::Dispatch { field: 2, target_idx: 0, cwd, prompt, followup: None }
            if cwd == "/tmp/dice" && prompt.is_empty()
        ));
        // Empty prompt keeps popup open.
        h.press(code(KeyCode::Enter));
        assert!(matches!(h.app.popup, Popup::Dispatch { .. }));
        assert!(h.toast().contains("enter a prompt"));

        // Type prompt, switch to field 0 (Tab), step target Right, submit.
        for ch in "fix bug".chars() {
            h.press(key(ch));
        }
        h.press(code(KeyCode::Tab));
        assert!(matches!(h.app.popup, Popup::Dispatch { field: 0, .. }));
        h.press(code(KeyCode::Right));
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.dispatched, vec![("agy:architect".into(), "/tmp/dice".into(), "fix bug".into())]);
    }

    #[test]
    fn followup_from_main_and_report_view_calls_ctl() {
        let mut h = Harness::new();
        select(&mut h.app, "cc:dice");
        h.press(key('f'));
        assert!(matches!(
            &h.app.popup,
            Popup::Dispatch { field: 2, followup: Some((id, _)), .. } if id == "cc:dice"
        ));
        for ch in "continue".chars() {
            h.press(key(ch));
        }
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.followups, vec![("cc:dice".into(), "continue".into())]);

        // From ReportView via 'f'.
        h.scene.reports.push(boss_report(42, "cc:office", "need reply"));
        h.app.popup = Popup::ReportView { id: 42, only: None, inbox: true };
        h.press(key('f'));
        assert!(matches!(
            &h.app.popup,
            Popup::Dispatch { field: 2, followup: Some((id, _)), .. } if id == "cc:office"
        ));
        for ch in "proceed".chars() {
            h.press(key(ch));
        }
        h.press(code(KeyCode::Enter));
        assert_eq!(h.ctl.followups.last(), Some(&("cc:office".into(), "proceed".into())));
    }

    #[test]
    fn approve_all_popup_resolves_all_pending_permissions() {
        let mut h = Harness::new();
        h.app.permissions_enabled = true;
        h.scene.agents.get_mut("cc:dice").unwrap().pending_permission = Some("perm-2".into());
        h.press(key('A'));
        assert!(matches!(&h.app.popup, Popup::ConfirmApproveAll { items } if items.len() == 2));
        h.press(key('y'));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.calls.len(), 2);
        assert!(h.ctl.calls.contains(&"perm perm-1 allow".to_string()));
        assert!(h.ctl.calls.contains(&"perm perm-2 allow".to_string()));
        assert_eq!(h.toast(), "approved 2 permissions");
    }

    #[test]
    fn actions_palette_runs_reap_and_prune() {
        let mut h = Harness::new();
        h.press(key(' '));
        assert_eq!(h.app.popup, Popup::Actions { sel: 0 });
        h.press(key('R'));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.reaped, 1);
        assert!(h.toast().contains("reaped 2 orphan agy (~420 MB)"));

        h.press(key(' '));
        h.press(key('B'));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.pruned, 1);
        assert!(h.toast().contains("pruning"));
    }

    #[test]
    fn x_kills_idle_claude_agent_with_pid() {
        let mut h = Harness::new();
        let mut c = agent("cc:killme", AgentKind::Claude, None);
        c.label = "cc·killme".into();
        c.pid = Some(7777);
        c.state = AgentState::Idle;
        h.scene.agents.insert(c.id.clone(), c);
        select(&mut h.app, "cc:killme");
        h.press(key('x'));
        assert!(matches!(&h.app.popup, Popup::ConfirmCancel { id, pid: 7777, .. } if id == "cc:killme"));
        h.press(key('y'));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.killed, vec![7777]);
        assert!(h.toast().contains("SIGTERM"));
    }

    #[test]
    fn o_resumes_ag_agent_with_resume_id() {
        let mut h = Harness::new();
        let mut ag = agent("ag:abc123", AgentKind::Other, None);
        ag.label = "agy-cli".into();
        ag.cwd = Some("/tmp/repo".into());
        ag.resume_id = Some("abc123".into());
        h.scene.agents.insert(ag.id.clone(), ag);
        select(&mut h.app, "ag:abc123");
        h.press(key('o'));
        assert_eq!(h.ctl.calls, vec!["resume /tmp/repo abc123".to_string()]);
        assert!(h.toast().contains("agy --resume abc123"), "{}", h.toast());
    }

    #[test]
    fn dispatch_targets_include_new_roles_and_rosso_auto() {
        let keys: Vec<&str> = DISPATCH_TARGETS.iter().map(|(k, _)| *k).collect();
        for expected in ["agy:manager", "agy:git", "rosso-auto:council", "rosso-auto:simple"] {
            assert!(keys.contains(&expected), "missing {expected} in {keys:?}");
        }
    }

    #[test]
    fn jobs_tab_r_resumes_and_x_aborts_auto_job_with_confirm() {
        let mut h = Harness::new();
        // Put one non-auto report job (older) and one auto job (newer).
        h.scene.reports.push(office_core::Report {
            id: 1,
            at_ms: 10,
            kind: office_core::ReportKind::Report,
            from: "rosso:A".into(),
            from_label: "A".into(),
            to: Some("rosso:B".into()),
            to_label: "B".into(),
            title: "Memo: plain".into(),
            body: "x".into(),
            brief: String::new(),
        });
        h.scene.report_jobs.insert(1, "plain-job".into());
        h.scene.auto_jobs.push(office_core::AutoJob {
            job: "auto-job".into(),
            task: "ship feature".into(),
            flow: "council".into(),
            phase: "execute".into(),
            status: "running".into(),
            calls_used: 4,
            max_calls: 12,
            started_at: 50,
            ..Default::default()
        });

        h.press(key('2'));
        assert_eq!(h.app.side_tab, SideTab::Jobs);
        assert_eq!(h.app.jobs_sel, 0);

        // Press R on auto-job -> calls resume_auto.
        h.press(key('R'));
        assert_eq!(*h.ctl.auto_resumed.lock().unwrap(), vec!["auto-job".to_string()]);
        assert_eq!(h.toast(), "resumed auto job auto-job");

        // Press X on auto-job -> opens ConfirmAbortAuto, Esc cancels.
        h.press(key('X'));
        assert_eq!(
            h.app.popup,
            Popup::ConfirmAbortAuto { job: "auto-job".into(), phase: "execute".into() }
        );
        h.press(code(KeyCode::Esc));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.ctl.auto_aborted.lock().unwrap().is_empty());

        // Press X then N cancels too.
        h.press(key('X'));
        h.press(key('n'));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.ctl.auto_aborted.lock().unwrap().is_empty());

        // Press X then Y confirms abort.
        h.press(key('X'));
        h.press(key('y'));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(*h.ctl.auto_aborted.lock().unwrap(), vec!["auto-job".to_string()]);
        assert_eq!(h.toast(), "abort requested for auto-job");

        // Press X then Enter also confirms abort.
        h.press(key('X'));
        h.press(code(KeyCode::Enter));
        assert_eq!(h.app.popup, Popup::None);
        assert_eq!(h.ctl.auto_aborted.lock().unwrap().len(), 2);

        // Error surfaces Critical toast.
        h.ctl.fail = true;
        h.press(key('R'));
        assert!(h.toast().starts_with("resume failed: boom"), "{}", h.toast());
        h.ctl.fail = false;

        // Selecting the non-auto job (index 1): R / X show a friendly toast, no popup.
        h.press(key('j'));
        assert_eq!(h.app.jobs_sel, 1);
        h.press(key('R'));
        assert!(h.toast().contains("not a rosso-auto job"), "{}", h.toast());
        h.press(key('X'));
        assert_eq!(h.app.popup, Popup::None);
        assert!(h.toast().contains("not a rosso-auto job"), "{}", h.toast());
    }

    #[test]
    fn preset_cycle_discovers_all_builtins_and_custom_themes() {
        let mut h = Harness::new();
        h.app.presets.insert("custom-mix".into(), uniform_mix("nord"));
        let themes = vec![
            "procedural".into(),
            "nord".into(),
            "tokyo-night".into(),
            "retro-amber".into(),
            "my-user-theme".into(),
        ];
        let cycle = h.app.preset_cycle(&themes);
        assert_eq!(
            cycle,
            vec!["procedural", "tokyo-night", "nord", "retro-amber", "custom-mix", "my-user-theme"]
        );
    }

    #[test]
    fn view_clean_preset_and_z_scale_toggle() {
        let mut h = Harness::new();
        assert!(!h.app.show.fill_viewport);
        h.press(key('z'));
        assert!(h.app.show.fill_viewport);
        assert!(h.view.show.fill_viewport);
        assert!(h.toast().contains("fill viewport"));

        h.press(key('v'));
        h.press(key('c'));
        assert!(h.app.show.selected_only_labels);
        assert!(h.app.show.names && h.app.show.lighting && h.app.show.theme_chrome);
        assert!(h.app.show.fill_viewport, "clean preset preserves studio visual prefs");
        h.press(code(KeyCode::Esc));

        h.press(key('z'));
        assert!(!h.app.show.fill_viewport);
        assert!(h.toast().contains("pixel-exact"));
    }
}
