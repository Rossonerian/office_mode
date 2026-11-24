//! Drawing: header, office (ratatui-image), sidebar (agents / router / alerts), footer, toasts, popups.
use super::fmt::*;
use super::state::{layer_get, App, Popup, LAYERS};
use super::{guarded, OfficeView};
use office_core::{model::AgentKind, Agent, AlertLevel, Scene};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, List, ListItem, Paragraph, Row, Table, Wrap},
    Frame,
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::{kitty::Kitty, Protocol},
    Image, Resize,
};

pub const SIDEBAR_W: u16 = 42;
const MAX_PX_W: u32 = 3840;
const MAX_PX_H: u32 = 2400;

/// The office widget: a renderer (B) plus an image protocol picker. Either may be missing,
/// in which case a text office is drawn instead.
pub struct OfficeCtx {
    pub view: Option<Box<dyn OfficeView>>,
    pub picker: Option<Picker>,
    pub is_tmux: bool,
    kitty_flip: bool,
    /// Last renderer failure (shown in the text fallback).
    pub error: Option<String>,
}

impl OfficeCtx {
    pub fn new(view: Option<Box<dyn OfficeView>>, picker: Option<Picker>) -> Self {
        OfficeCtx { view, picker, is_tmux: std::env::var_os("TMUX").is_some(), kitty_flip: false, error: None }
    }

    pub fn font_px(&self) -> Option<(u16, u16)> {
        if self.view.is_some() {
            self.picker.as_ref().map(|p| p.font_size())
        } else {
            None
        }
    }

    /// Compose the scene at the area's pixel size and wrap it in an image protocol.
    fn frame(&mut self, app: &App, scene: &Scene, area: Rect, t_ms: u64) -> Result<Protocol, String> {
        let (Some(view), Some(picker)) = (self.view.as_mut(), self.picker.as_ref()) else {
            return Err("no renderer".into());
        };
        let (fw, fh) = picker.font_size();
        let (fw, fh) = (fw.max(1) as u32, fh.max(1) as u32);
        let px_w = (area.width as u32 * fw).min(MAX_PX_W);
        let px_h = (area.height as u32 * fh).min(MAX_PX_H);
        let cells = Rect::new(area.x, area.y, (px_w / fw) as u16, (px_h / fh) as u16);
        let sel = app.selected.clone();
        view.set_hover(app.hover.clone());
        let img = guarded(|| view.compose(scene, sel.as_ref(), px_w, px_h, t_ms))?;
        let dynimg = image::DynamicImage::ImageRgba8(img);
        match picker.protocol_type() {
            ProtocolType::Kitty => {
                // Alternate two fixed image ids so Kitty replaces old frames instead of piling them up.
                self.kitty_flip = !self.kitty_flip;
                let id = if self.kitty_flip { 0x00A6_0F01 } else { 0x00A6_0F02 };
                Kitty::new(dynimg, cells, id, self.is_tmux).map(Protocol::Kitty).map_err(|e| e.to_string())
            }
            _ => picker.new_protocol(dynimg, cells, Resize::Fit(None)).map_err(|e| e.to_string()),
        }
    }
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

pub fn draw(f: &mut Frame, app: &mut App, scene: &Scene, office: &mut OfficeCtx, now_ms: u64) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    draw_header(f, app, scene, header);
    draw_footer(f, footer);

    let (office_area, side_area) = if app.sidebar && body.width > SIDEBAR_W + 10 {
        let [o, s] = Layout::horizontal([Constraint::Min(10), Constraint::Length(SIDEBAR_W)]).areas(body);
        (o, Some(s))
    } else {
        (body, None)
    };
    app.layout.office = office_area;
    app.layout.font_px = office.font_px();
    draw_office(f, app, scene, office, office_area, now_ms);
    match side_area {
        Some(s) => draw_sidebar(f, app, scene, s, now_ms),
        None => {
            app.layout.row_ids.clear();
            app.layout.agent_rows = Rect::default();
        }
    }
    draw_toast(f, app, office_area);
    draw_popup(f, app, scene, office, now_ms);
}

fn draw_header(f: &mut Frame, app: &App, scene: &Scene, area: Rect) {
    let (c, s, g, o) = kind_counts(scene);
    let total = c + s + g + o;
    let mut spans = vec![
        Span::styled(" agent office ", Style::new().bold().fg(Color::Black).bg(Color::Rgb(0xE0, 0x7A, 0x3F))),
        Span::raw(format!(" {total} agents (")),
        Span::styled(format!("claude {c}"), Style::new().fg(kind_color(AgentKind::Claude))),
        Span::raw(" · "),
        Span::styled(format!("subs {s}"), Style::new().fg(kind_color(AgentKind::ClaudeSub))),
        Span::raw(" · "),
        Span::styled(format!("agy {g}"), Style::new().fg(kind_color(AgentKind::Agy))),
        Span::raw(format!(")  │ tokens {}  │ {}", fmt_tokens(scene.total_tokens), fmt_cost(scene.total_cost_usd))),
    ];
    if !app.notifications {
        spans.push(Span::styled("  │ notif off", Style::new().fg(Color::DarkGray)));
    }
    if app.demo {
        spans.push(Span::styled("  │ DEMO", Style::new().fg(Color::Yellow).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub const FOOTER_HINT: &str =
    " q quit  ↑↓/jk select  ⏎ detail  a/d approve/deny  x cancel agy  o resume  t/T theme  s sidebar  n notif  c clear  ? help";

fn draw_footer(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(trunc(FOOTER_HINT, area.width as usize)).style(Style::new().fg(Color::DarkGray)),
        area,
    );
}

fn draw_office(f: &mut Frame, app: &App, scene: &Scene, office: &mut OfficeCtx, area: Rect, now_ms: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if office.view.is_some() && office.picker.is_some() {
        match office.frame(app, scene, area, now_ms) {
            Ok(proto) => {
                office.error = None;
                f.render_widget(Image::new(&proto), area);
                return;
            }
            Err(e) => office.error = Some(e),
        }
    }
    draw_text_office(f, app, scene, office.error.as_deref(), area);
}

/// Fallback when no renderer / image protocol is available: one line per agent.
fn draw_text_office(f: &mut Frame, app: &App, scene: &Scene, err: Option<&str>, area: Rect) {
    let mut lines = Vec::new();
    for (id, depth) in agent_rows(scene) {
        let Some(a) = scene.agents.get(&id) else { continue };
        let sel = app.selected.as_deref() == Some(id.as_str());
        let mark = if sel { "▶ " } else { "  " };
        lines.push(Line::from(vec![
            Span::raw(mark),
            Span::styled(format!("{:<22}", trunc(&tree_label(&a.label, depth), 22)), Style::new().fg(kind_color(a.kind))),
            Span::styled(format!("{:<11}", state_name(a.state)), Style::new().fg(state_color(a.state))),
            Span::raw(a.activity.clone()),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  the office is empty: no agents yet", Style::new().fg(Color::DarkGray))));
    }
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(format!("  (text mode: {e})"), Style::new().fg(Color::DarkGray))));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" office ")).wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_sidebar(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    let alerts_h = (area.height / 3).clamp(4, 10);
    let [agents, router, alerts] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(6), Constraint::Length(alerts_h)]).areas(area);
    draw_agents(f, app, scene, agents, now_ms);
    draw_router(f, scene, router);
    draw_alerts(f, app, scene, alerts, now_ms);
}

pub const AGENT_COLS: [u16; 6] = [10, 7, 5, 4, 5, 4];

pub fn agent_cells(a: &Agent, depth: usize, now_ms: u64) -> [String; 6] {
    [
        trunc(&tree_label(&a.label, depth), AGENT_COLS[0] as usize),
        trunc(&role_or_model(a), AGENT_COLS[1] as usize),
        state_short(a.state).to_string(),
        fmt_elapsed_short(now_ms.saturating_sub(a.started_ms)),
        fmt_tokens(a.tokens_in + a.tokens_out),
        fmt_cost_short(a.cost_usd),
    ]
}

fn draw_agents(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    let rows = agent_rows(scene);
    let block = Block::bordered().title(format!(" Agents ({}) ", rows.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    let cap = body.height as usize;
    let sel_idx = app.selected.as_ref().and_then(|s| rows.iter().position(|(id, _)| id == s)).unwrap_or(0);
    let offset = if sel_idx >= cap { sel_idx + 1 - cap } else { 0 };
    let visible: Vec<&(String, usize)> = rows.iter().skip(offset).take(cap).collect();
    app.layout.agent_rows = body;
    app.layout.row_ids = visible.iter().map(|(id, _)| id.clone()).collect();

    let header = Row::new(["agent", "role", "state", "time", "tok", "$"]).style(Style::new().fg(Color::DarkGray).bold());
    let trows: Vec<Row> = visible
        .iter()
        .filter_map(|(id, depth)| scene.agents.get(id).map(|a| (a, *depth)))
        .map(|(a, depth)| {
            let c = agent_cells(a, depth, now_ms);
            let mut st = Style::new().fg(state_color(a.state));
            if a.state == office_core::AgentState::NeedsYou {
                st = st.add_modifier(Modifier::BOLD);
            }
            let row = Row::new(vec![
                Cell::from(c[0].clone()).style(Style::new().fg(kind_color(a.kind))),
                Cell::from(c[1].clone()),
                Cell::from(c[2].clone()).style(st),
                Cell::from(c[3].clone()),
                Cell::from(c[4].clone()),
                Cell::from(c[5].clone()),
            ]);
            if app.selected.as_deref() == Some(a.id.as_str()) {
                row.style(Style::new().bg(Color::Rgb(0x33, 0x3A, 0x4A)).add_modifier(Modifier::BOLD))
            } else {
                row
            }
        })
        .collect();
    let widths = AGENT_COLS.map(Constraint::Length);
    let table = Table::new(trows, widths).header(header).column_spacing(1);
    f.render_widget(table, inner);
}

/// Router text lines (without the gauge), for the sidebar and tests.
pub fn router_lines(scene: &Scene, width: usize) -> Vec<Line<'static>> {
    let r = &scene.router;
    let free = r.free_mb.map(|v| format!("{v} MB")).unwrap_or_else(|| "?".into());
    let per = r.per_proc_mb.map(|v| format!("{v} MB")).unwrap_or_else(|| "?".into());
    let (pl, pc) = pressure_label(r.pressure);
    let limit = r.limit_text.clone().unwrap_or_else(|| "none".into());
    vec![
        Line::raw(trunc(&format!("free RAM {free} · per agy {per}"), width)),
        Line::from(vec![
            Span::raw("pressure "),
            Span::styled(pl, Style::new().fg(pc).bold()),
            Span::raw(format!(" · queued {}", r.queued)),
        ]),
        Line::from(Span::styled(trunc(&format!("LIMIT {limit}"), width), Style::new().fg(Color::DarkGray))),
    ]
}

pub fn router_gauge_label(scene: &Scene) -> (String, f64) {
    let r = &scene.router;
    match r.capacity {
        Some(cap) if cap > 0 => (format!("busy {}/{}", r.busy_slots, cap), (r.busy_slots as f64 / cap as f64).clamp(0.0, 1.0)),
        _ => (format!("busy {}/?", r.busy_slots), 0.0),
    }
}

fn draw_router(f: &mut Frame, scene: &Scene, area: Rect) {
    let block = Block::bordered().title(" Router ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let [g, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let (label, ratio) = router_gauge_label(scene);
    let color = if ratio >= 1.0 { Color::LightRed } else if ratio >= 0.67 { Color::Yellow } else { Color::Green };
    f.render_widget(Gauge::default().ratio(ratio).label(label).gauge_style(Style::new().fg(color).bg(Color::Rgb(0x22, 0x22, 0x22))), g);
    f.render_widget(Paragraph::new(router_lines(scene, inner.width as usize)), rest);
}

pub fn alert_line(now_ms: u64, al: &office_core::Alert, scene: &Scene, width: usize) -> String {
    let who = al
        .agent
        .as_ref()
        .map(|id| scene.agents.get(id).map(|a| a.label.clone()).unwrap_or_else(|| id.clone()))
        .map(|l| format!(" {l}"))
        .unwrap_or_default();
    trunc(&format!("{:>4} {} {}{who}: {}", fmt_ago(now_ms, al.at_ms), level_tag(al.level), al.code, al.message), width)
}

fn draw_alerts(f: &mut Frame, app: &App, scene: &Scene, area: Rect, now_ms: u64) {
    let alerts = app.visible_alerts(scene);
    let block = Block::bordered().title(format!(" Alerts ({}) ", alerts.len()));
    let width = block.inner(area).width as usize;
    let items: Vec<ListItem> = alerts
        .iter()
        .map(|al| ListItem::new(alert_line(now_ms, al, scene, width)).style(Style::new().fg(level_color(al.level))))
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

fn draw_toast(f: &mut Frame, app: &App, office: Rect) {
    let Some(t) = app.current_toast() else { return };
    if office.height < 3 {
        return;
    }
    let w = (t.text.chars().count() as u16 + 4).min(office.width.saturating_sub(2));
    let r = Rect::new(office.x + 1, office.y + office.height - 2, w, 1);
    let bg = match t.level {
        AlertLevel::Info => Color::Rgb(0x2A, 0x3A, 0x55),
        AlertLevel::Warn => Color::Rgb(0x66, 0x55, 0x10),
        AlertLevel::Critical => Color::Rgb(0x77, 0x18, 0x18),
    };
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(format!(" {} ", trunc(&t.text, w.saturating_sub(2) as usize))).style(Style::new().bg(bg).fg(Color::White)), r);
}

pub const HELP: &[(&str, &str)] = &[
    ("q / ctrl-c", "quit"),
    ("↑↓ / j k, g G", "select agent (or click a row / an agent)"),
    ("enter", "agent details"),
    ("a / d", "approve / deny the pending permission"),
    ("x", "cancel the selected agy call (asks y/n)"),
    ("o", "resume the selected Claude session"),
    ("t", "cycle theme presets"),
    ("T", "theme mixer (per layer)"),
    ("s", "toggle sidebar"),
    ("n", "toggle notifications"),
    ("c", "clear alerts"),
    ("?", "this help"),
];

pub fn detail_lines(app: &App, a: &Agent, scene: &Scene, now_ms: u64) -> Vec<Line<'static>> {
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    let parent = a
        .parent
        .as_ref()
        .map(|p| scene.agents.get(p).map(|x| format!("{} ({p})", x.label)).unwrap_or_else(|| p.clone()))
        .unwrap_or_else(|| "-".into());
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<12}"), Style::new().fg(Color::DarkGray)), Span::raw(v)]);
    let mut v = vec![
        kv("id", a.id.clone()),
        kv("kind", format!("{:?}", a.kind)),
        Line::from(vec![
            Span::styled(format!("{:<12}", "state"), Style::new().fg(Color::DarkGray)),
            Span::styled(state_name(a.state).to_string(), Style::new().fg(state_color(a.state)).bold()),
        ]),
        kv("activity", if a.activity.is_empty() { "-".into() } else { a.activity.clone() }),
        kv("parent", parent),
        kv("cwd", opt(&a.cwd)),
        kv("role", opt(&a.role)),
        kv("model", opt(&a.model)),
        kv("slot", a.slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into())),
        kv("pid", a.pid.map(|s| s.to_string()).unwrap_or_else(|| "-".into())),
        kv("elapsed", fmt_elapsed_long(now_ms.saturating_sub(a.started_ms))),
        kv("last event", format!("{} ago", fmt_ago(now_ms, a.last_event_ms))),
        kv("tokens", format!("{} in · {} out", fmt_tokens(a.tokens_in), fmt_tokens(a.tokens_out))),
        kv("cost", fmt_cost(a.cost_usd)),
        kv("context", a.context_pct.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "-".into())),
        kv("rss", a.rss_mb.map(|m| format!("{m} MB")).unwrap_or_else(|| "-".into())),
        kv("tools", format!("{} calls · {} failed", a.tool_calls, a.tool_failures)),
        kv("resume id", opt(&a.resume_id)),
        kv("permission", opt(&a.pending_permission)),
        Line::raw(""),
        Line::from(Span::styled("recent activity", Style::new().bold())),
    ];
    match app.history.get(&a.id) {
        Some(h) if !h.is_empty() => {
            for (t, text) in h.iter().rev() {
                v.push(Line::raw(format!("  {:>4}  {text}", fmt_ago(now_ms, *t))));
            }
        }
        _ => v.push(Line::raw("  (none yet)")),
    }
    v
}

fn popup_frame(f: &mut Frame, r: Rect, title: String) -> Rect {
    f.render_widget(Clear, r);
    let b = Block::bordered().borders(Borders::ALL).title(title).style(Style::new().bg(Color::Rgb(0x16, 0x18, 0x20)));
    let inner = b.inner(r);
    f.render_widget(b, r);
    inner
}

fn draw_popup(f: &mut Frame, app: &App, scene: &Scene, office: &OfficeCtx, now_ms: u64) {
    let area = f.area();
    match &app.popup {
        Popup::None => {}
        Popup::Help => {
            let r = centered(area, 60, HELP.len() as u16 + 4);
            let inner = popup_frame(f, r, " help ".into());
            let mut lines: Vec<Line> = HELP
                .iter()
                .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<16}"), Style::new().fg(Color::Yellow)), Span::raw(*d)]))
                .collect();
            lines.push(Line::from(Span::styled("esc / ? to close", Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::Detail(id) => {
            let Some(a) = scene.agents.get(id) else { return };
            let lines = detail_lines(app, a, scene, now_ms);
            let r = centered(area, 72, lines.len() as u16 + 2);
            let inner = popup_frame(f, r, format!(" {} ", a.label));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        Popup::ConfirmCancel { label, pid, .. } => {
            let text = format!("Cancel {label} (pid {pid})? y/n");
            let r = centered(area, text.chars().count() as u16 + 6, 3);
            let inner = popup_frame(f, r, " confirm ".into());
            f.render_widget(Paragraph::new(text).style(Style::new().fg(Color::LightRed).bold()), inner);
        }
        Popup::Mixer { layer } => {
            let themes = office.view.as_ref().map(|v| v.themes()).unwrap_or_else(|| vec!["procedural".into()]);
            let r = centered(area, 50, LAYERS.len() as u16 + 5);
            let inner = popup_frame(f, r, " theme mixer ".into());
            let mut lines: Vec<Line> = LAYERS
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let cur = layer_get(&app.mix, i).to_string();
                    let style = if i == *layer { Style::new().fg(Color::Yellow).bold() } else { Style::new() };
                    let mark = if i == *layer { "▶" } else { " " };
                    Line::from(Span::styled(format!("{mark} {l:<12} ◀ {cur} ▶"), style))
                })
                .collect();
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(trunc(&format!("available: {}", themes.join(", ")), inner.width as usize), Style::new().fg(Color::DarkGray))));
            lines.push(Line::from(Span::styled("↑↓ layer  ←→ theme  esc save & close", Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        let buf = t.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn render(app: &mut App, scene: &Scene, office: &mut OfficeCtx, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app, scene, office, 65_000)).unwrap();
        buffer_text(&t)
    }

    fn test_app() -> (App, Scene) {
        let cfg = office_core::config::Config::default();
        let mut app = App::new(&cfg, false);
        app.config_path = None;
        let scene = demo_scene();
        app.observe(&scene, 1_000, &mut FakeControl::default());
        (app, scene)
    }

    #[test]
    fn sidebar_snapshot_has_key_strings() {
        let (mut app, scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        let s = render(&mut app, &scene, &mut office, 120, 40);
        for needle in [
            "Agents (4)", "cc·DICE", "└Explore", "agy-worker", "NEED!", "read", "Router", "busy 2/3", "free RAM 1800 MB",
            "per agy 280 MB", "pressure warn", "queued 1", "LIMIT capacity 3 reached", "Alerts (2)", "WARN loop", "info router",
            "4 agents (claude 2", "subs 1", "agy 1", "tokens 15.1k", "$1.23", "q quit",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in:\n{s}");
        }
        // Newest alert first.
        assert!(s.find("WARN loop").unwrap() < s.find("info router").unwrap());
        // Row hit-test geometry was recorded for all four agents.
        assert_eq!(app.layout.row_ids.len(), 4);
        assert_eq!(app.layout.agent_rows.x, 120 - SIDEBAR_W + 1);
    }

    #[test]
    fn sidebar_toggle_hides_table() {
        let (mut app, scene) = test_app();
        app.sidebar = false;
        let mut office = OfficeCtx::new(None, None);
        let s = render(&mut app, &scene, &mut office, 120, 30);
        assert!(!s.contains("Router"));
        assert!(app.layout.row_ids.is_empty());
        // text-mode office lists agents
        assert!(s.contains("needs you"));
    }

    #[test]
    fn office_renders_through_halfblocks_with_fake_renderer() {
        let (mut app, scene) = test_app();
        let mut picker = Picker::from_fontsize((8, 16));
        picker.set_protocol_type(ProtocolType::Halfblocks);
        let view = FakeView::new(&["procedural"]);
        let calls = view.composed.clone();
        let mut office = OfficeCtx::new(Some(Box::new(view)), Some(picker));
        let _ = render(&mut app, &scene, &mut office, 100, 30);
        let (w, h) = *calls.lock().unwrap().last().expect("compose called");
        // office area = 100 - 42 cols, 30 - 2 rows, at 8x16 px per cell
        assert_eq!((w, h), (58 * 8, 28 * 16));
        assert!(office.error.is_none());
        assert_eq!(app.layout.font_px, Some((8, 16)));
    }

    #[test]
    fn popups_render() {
        let (mut app, scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        app.popup = Popup::Help;
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("theme mixer (per layer)"));
        app.popup = Popup::Detail("cc:office".into());
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("perm-1") && s.contains("recent activity") && s.contains("/tmp/office"), "{s}");
        app.popup = Popup::ConfirmCancel { id: "agy:4242".into(), label: "agy-worker".into(), pid: 4242 };
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("Cancel agy-worker (pid 4242)? y/n"));
        app.popup = Popup::Mixer { layer: 2 };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("▶ furniture") && s.contains("◀ procedural ▶"), "{s}");
    }

    #[test]
    fn toast_and_formatting_helpers() {
        let (mut app, scene) = test_app();
        app.toast("theme: cozy", AlertLevel::Info, 64_000);
        let mut office = OfficeCtx::new(None, None);
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("theme: cozy"));
        let a = &scene.agents["agy:4242"];
        let c = agent_cells(a, 1, 65_000);
        assert_eq!(c[0], "└agy-work…");
        assert_eq!(c[1], "worker");
        assert_eq!(c[2], "type");
        assert_eq!(c[3], "15s");
        assert_eq!(router_gauge_label(&scene), ("busy 2/3".to_string(), 2.0 / 3.0));
        let line = alert_line(65_000, &scene.alerts[1], &scene, 40);
        assert!(line.chars().count() <= 40);
        assert!(line.starts_with("  5s WARN loop"), "{line}");
    }
}
