//! Drawing: header, office (compressed Kitty / direct halfblocks / ratatui-image), sidebar, footer, popups.
use super::fmt::*;
use super::kitty;
use super::state::{layer_get, App, Popup, LAYERS};
use super::{guarded, OfficeView};
use crate::render;
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
    protocol::Protocol,
    Image, Resize,
};

pub const SIDEBAR_W: u16 = 42;
const MAX_PX_W: u32 = 3840;
const MAX_PX_H: u32 = 2400;

/// Adaptive sidebar width based on terminal body width.
pub fn sidebar_width_for(body_width: u16) -> Option<u16> {
    if body_width >= 96 {
        Some(SIDEBAR_W)
    } else if body_width >= 78 {
        Some(34)
    } else {
        None
    }
}

/// Compute the office pane rect for a terminal area and sidebar state.
pub fn office_rect(area: Rect, sidebar: bool) -> Rect {
    let [_, body, _] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(area);
    if sidebar {
        if let Some(sw) = sidebar_width_for(body.width) {
            let [o, _] = Layout::horizontal([Constraint::Min(10), Constraint::Length(sw)]).areas(body);
            return o;
        }
    }
    body
}

/// The office widget: a renderer (B) plus an image protocol picker. Either may be missing,
/// in which case a text office is drawn instead.
pub struct OfficeCtx {
    pub view: Option<Box<dyn OfficeView>>,
    pub picker: Option<Picker>,
    pub is_tmux: bool,
    kitty_flip: bool,
    active_kitty_id: u32,
    pending_kitty_bytes: Option<Vec<u8>>,
    last_logical: Vec<u8>,
    last_logical_dims: (u32, u32),
    last_area: Rect,
    last_px: (u32, u32),
    cached_proto: Option<Protocol>,
    scaled_scratch: image::RgbaImage,
    rgb_scratch: Vec<u8>,
    xmap: Vec<usize>,
    /// Last renderer failure (shown in the text fallback).
    pub error: Option<String>,
    /// A frame was already prepared this tick (avoid composing twice).
    prepared: bool,
    /// Whether the last composed frame had anyone mid-walk.
    pub last_walking: bool,
}

impl OfficeCtx {
    pub fn new(view: Option<Box<dyn OfficeView>>, picker: Option<Picker>) -> Self {
        OfficeCtx {
            view,
            picker,
            is_tmux: std::env::var_os("TMUX").is_some(),
            kitty_flip: false,
            active_kitty_id: 0,
            pending_kitty_bytes: None,
            last_logical: Vec::new(),
            last_logical_dims: (0, 0),
            last_area: Rect::default(),
            last_px: (0, 0),
            cached_proto: None,
            scaled_scratch: image::RgbaImage::new(1, 1),
            rgb_scratch: Vec::new(),
            xmap: Vec::new(),
            error: None,
            prepared: false,
            last_walking: false,
        }
    }

    pub fn font_px(&self) -> Option<(u16, u16)> {
        if self.view.is_some() {
            self.picker.as_ref().map(|p| p.font_size())
        } else {
            None
        }
    }

    pub fn take_pending_kitty(&mut self) -> Option<Vec<u8>> {
        self.pending_kitty_bytes.take()
    }

    /// Compose & encode the next frame if anything changed (reusing cached bytes/protocols when unchanged).
    pub fn prepare_frame(&mut self, app: &App, scene: &Scene, area: Rect, t_ms: u64) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (Some(view), Some(picker)) = (self.view.as_mut(), self.picker.as_ref()) else {
            return;
        };
        let proto_ty = picker.protocol_type();
        if proto_ty == ProtocolType::Halfblocks {
            // Halfblocks renders directly from compose_logical inside draw_office.
            return;
        }
        let (fw, fh) = picker.font_size();
        let (fw, fh) = (fw.max(1) as u32, fh.max(1) as u32);
        let px_w = (area.width as u32 * fw).min(MAX_PX_W);
        let px_h = (area.height as u32 * fh).min(MAX_PX_H);
        let cells = Rect::new(area.x, area.y, (px_w / fw) as u16, (px_h / fh) as u16);
        let sel = app.selected.clone();
        view.set_hover(app.hover.clone());

        if let Some(lf) = view.compose_logical(scene, sel.as_ref(), px_w, px_h, t_ms) {
            self.prepared = true;
            self.last_walking = lf.is_walking;
            let dims = lf.img.dimensions();
            let unchanged = self.last_area == cells
                && self.last_px == (px_w, px_h)
                && self.last_logical_dims == dims
                && self.last_logical == *lf.img.as_raw();
            if unchanged {
                if (proto_ty == ProtocolType::Kitty && self.active_kitty_id != 0)
                    || (proto_ty != ProtocolType::Kitty && self.cached_proto.is_some())
                {
                    self.error = None;
                    return;
                }
            }
            self.last_area = cells;
            self.last_px = (px_w, px_h);
            self.last_logical_dims = dims;
            self.last_logical.clear();
            self.last_logical.extend_from_slice(lf.img.as_raw());

            if self.scaled_scratch.dimensions() != (px_w, px_h) {
                self.scaled_scratch = image::RgbaImage::new(px_w, px_h);
            }
            for px in self.scaled_scratch.as_mut().chunks_exact_mut(4) {
                px.copy_from_slice(&lf.backdrop);
            }
            render::canvas::scale_into_with_xmap(&mut self.scaled_scratch, lf.img, lf.ox, lf.oy, lf.scale, &mut self.xmap);

            match proto_ty {
                ProtocolType::Kitty => {
                    self.kitty_flip = !self.kitty_flip;
                    let next_id = if self.kitty_flip { kitty::ID_A } else { kitty::ID_B };
                    let bytes = kitty::encode_rgb_zlib(&self.scaled_scratch, next_id, cells, self.is_tmux, &mut self.rgb_scratch);
                    self.active_kitty_id = next_id;
                    self.pending_kitty_bytes = Some(bytes);
                    self.error = None;
                }
                _ => {
                    let dynimg = image::DynamicImage::ImageRgba8(self.scaled_scratch.clone());
                    match picker.new_protocol(dynimg, cells, Resize::Fit(None)) {
                        Ok(p) => {
                            self.cached_proto = Some(p);
                            self.error = None;
                        }
                        Err(e) => {
                            self.cached_proto = None;
                            self.error = Some(e.to_string());
                        }
                    }
                }
            }
            return;
        }

        // Fallback when view doesn't support compose_logical (e.g. FakeView).
        match guarded(|| view.compose(scene, sel.as_ref(), px_w, px_h, t_ms)) {
            Ok(img) => match proto_ty {
                ProtocolType::Kitty => {
                    self.kitty_flip = !self.kitty_flip;
                    let next_id = if self.kitty_flip { kitty::ID_A } else { kitty::ID_B };
                    let bytes = kitty::encode_rgb_zlib(&img, next_id, cells, self.is_tmux, &mut self.rgb_scratch);
                    self.active_kitty_id = next_id;
                    self.pending_kitty_bytes = Some(bytes);
                    self.error = None;
                }
                _ => {
                    let dynimg = image::DynamicImage::ImageRgba8(img);
                    match picker.new_protocol(dynimg, cells, Resize::Fit(None)) {
                        Ok(p) => {
                            self.cached_proto = Some(p);
                            self.error = None;
                        }
                        Err(e) => {
                            self.cached_proto = None;
                            self.error = Some(e.to_string());
                        }
                    }
                }
            },
            Err(e) => {
                self.cached_proto = None;
                self.error = Some(e);
            }
        }
    }
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

pub fn popup_rect(area: Rect, app: &App, scene: &Scene) -> Option<Rect> {
    let max_h = area.height.saturating_sub(2).max(5);
    match &app.popup {
        Popup::None => None,
        Popup::Help => Some(centered(area, 62, (HELP.len() as u16 + 4).min(max_h))),
        Popup::Detail(id) => scene.agents.get(id).map(|a| {
            let lines = detail_lines(app, a, scene, scene.now_ms);
            centered(area, 72, (lines.len() as u16 + 2).min(max_h))
        }),
        Popup::ConfirmCancel { label, pid, .. } => {
            let text = format!("Cancel {label} (pid {pid})? y/n");
            Some(centered(area, text.chars().count() as u16 + 6, 3))
        }
        Popup::Mixer { .. } => Some(centered(area, 50, (LAYERS.len() as u16 + 5).min(max_h))),
        Popup::Reports { only, .. } => {
            let n = crate::app::state::report_list(scene, only.as_deref()).len() as u16;
            Some(centered(area, 100, (n + 4).clamp(6, max_h)))
        }
        Popup::ReportView { .. } => Some(centered(area, 100, max_h)),
    }
}

fn report_kind_tag(k: office_core::ReportKind) -> (&'static str, Color) {
    match k {
        office_core::ReportKind::Task => ("brief ", Color::Gray),
        office_core::ReportKind::Report => ("report", Color::Yellow),
        office_core::ReportKind::Result => ("result", Color::LightBlue),
    }
}

/// The full text of one hand-off: who, when, the brief the sender was given, and what it sent.
pub fn report_lines(r: &office_core::Report, now_ms: u64) -> Vec<Line<'static>> {
    let (tag, c) = report_kind_tag(r.kind);
    let dim = Style::new().fg(Color::DarkGray);
    let mut v = vec![
        Line::from(vec![
            Span::styled(tag.trim().to_string(), Style::new().fg(c).bold()),
            Span::raw(format!("  {} → {}", r.from_label, r.to_label)),
            Span::styled(format!("   {} ago", fmt_ago(now_ms, r.at_ms)), dim),
        ]),
        Line::from(Span::styled(r.title.clone(), Style::new().bold())),
    ];
    let mut section = |name: &str, text: &str| {
        if text.trim().is_empty() {
            return;
        }
        v.push(Line::raw(""));
        v.push(Line::from(Span::styled(format!("── {name} "), dim)));
        for l in text.lines() {
            v.push(Line::raw(l.to_string()));
        }
    };
    section("brief it was given", &r.brief);
    let body_name = match r.kind {
        office_core::ReportKind::Task => "brief",
        office_core::ReportKind::Report => "report",
        office_core::ReportKind::Result => "result",
    };
    section(body_name, &r.body);
    if r.brief.trim().is_empty() && r.body.trim().is_empty() {
        v.push(Line::raw(""));
        v.push(Line::from(Span::styled("(no text captured: the brief is filled in when the report comes back)", dim)));
    }
    v
}

pub fn draw(f: &mut Frame, app: &mut App, scene: &Scene, office: &mut OfficeCtx, now_ms: u64) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    draw_header(f, app, scene, header);
    draw_footer(f, app, scene, footer);

    let (office_area, side_area) = match app.sidebar.then(|| sidebar_width_for(body.width)).flatten() {
        Some(sw) => {
            let [o, s] = Layout::horizontal([Constraint::Min(10), Constraint::Length(sw)]).areas(body);
            (o, Some(s))
        }
        None => (body, None),
    };
    app.layout.office = office_area;
    app.layout.font_px = office.font_px();
    let occ = popup_rect(f.area(), app, scene);
    draw_office(f, app, scene, office, office_area, occ, now_ms);
    match side_area {
        Some(s) => draw_sidebar(f, app, scene, s, now_ms),
        None => {
            app.layout.row_ids.clear();
            app.layout.agent_rows = Rect::default();
            app.layout.alert_rows = Rect::default();
            app.layout.alert_agent_ids.clear();
        }
    }
    draw_popup(f, app, scene, office, now_ms);
}

fn theme_tag(mix: &office_core::config::ThemeMix) -> String {
    if mix.floor == mix.walls && mix.floor == mix.furniture && mix.floor == mix.decor && mix.floor == mix.characters {
        mix.floor.clone()
    } else {
        "mix".into()
    }
}

fn draw_header(f: &mut Frame, app: &App, scene: &Scene, area: Rect) {
    let (c, s, g, o) = kind_counts(scene);
    let total = c + s + g + o;
    let tag = theme_tag(&app.mix);
    let mut spans = if area.width >= 105 {
        vec![
            Span::styled(" ROSSO · agent office ", Style::new().bold().fg(Color::Black).bg(Color::Rgb(0xE0, 0x7A, 0x3F))),
            Span::raw(format!(" {total} agents (")),
            Span::styled(format!("claude {c}"), Style::new().fg(kind_color(AgentKind::Claude))),
            Span::raw(" · "),
            Span::styled(format!("subs {s}"), Style::new().fg(kind_color(AgentKind::ClaudeSub))),
            Span::raw(" · "),
            Span::styled(format!("agy {g}"), Style::new().fg(kind_color(AgentKind::Agy))),
            Span::raw(format!(")  │ tokens {} (excl. cache reads)  │ {}", fmt_tokens(scene.total_tokens), fmt_cost(scene.total_cost_usd))),
            Span::styled(format!("  │ [{tag}]"), Style::new().fg(Color::DarkGray)),
        ]
    } else {
        vec![
            Span::styled(" ROSSO ", Style::new().bold().fg(Color::Black).bg(Color::Rgb(0xE0, 0x7A, 0x3F))),
            Span::raw(format!(" {total}a (")),
            Span::styled(format!("{c}c"), Style::new().fg(kind_color(AgentKind::Claude))),
            Span::raw("/"),
            Span::styled(format!("{s}s"), Style::new().fg(kind_color(AgentKind::ClaudeSub))),
            Span::raw("/"),
            Span::styled(format!("{g}g"), Style::new().fg(kind_color(AgentKind::Agy))),
            Span::raw(format!(") │ {} │ {}", fmt_tokens(scene.total_tokens), fmt_cost(scene.total_cost_usd))),
            Span::styled(format!(" [{tag}]"), Style::new().fg(Color::DarkGray)),
        ]
    };
    if app.sound {
        spans.push(Span::styled("  │ sound", Style::new().fg(Color::Cyan)));
    }
    if !app.notifications {
        spans.push(Span::styled("  │ notif off", Style::new().fg(Color::DarkGray)));
    }
    if app.demo {
        spans.push(Span::styled("  │ DEMO", Style::new().fg(Color::Yellow).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, app: &App, scene: &Scene, area: Rect) {
    if let Some(t) = app.current_toast() {
        let bg = match t.level {
            AlertLevel::Info => Color::Rgb(0x2A, 0x3A, 0x55),
            AlertLevel::Warn => Color::Rgb(0x66, 0x55, 0x10),
            AlertLevel::Critical => Color::Rgb(0x77, 0x18, 0x18),
        };
        f.render_widget(
            Paragraph::new(format!(" {} ", trunc(&t.text, area.width.saturating_sub(2) as usize)))
                .style(Style::new().bg(bg).fg(Color::White)),
            area,
        );
        return;
    }
    let dim = Style::new().fg(Color::DarkGray);
    let mut spans = vec![Span::styled(" q quit  ↑↓/jk select  ⏎ detail", dim)];
    if let Some(a) = app.selected_agent(scene) {
        if a.pending_permission.is_some() || a.state == office_core::AgentState::NeedsYou {
            spans.push(Span::styled("  a/d approve/deny", Style::new().fg(Color::Yellow).bold()));
        }
        if a.kind == AgentKind::Agy {
            spans.push(Span::styled("  x cancel", Style::new().fg(Color::LightRed)));
        } else if matches!(a.kind, AgentKind::Claude | AgentKind::ClaudeSub) {
            spans.push(Span::styled("  o resume", Style::new().fg(Color::Cyan)));
        }
    }
    spans.push(Span::styled("  Tab jump  r reports  t/T theme  s side  ? help", dim));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_halfblocks_direct(buf: &mut ratatui::buffer::Buffer, area: Rect, lf: &render::LogicalFrame<'_>, fw: u32, fh: u32) {
    let (lw, lh) = (lf.img.width() as i32, lf.img.height() as i32);
    let raw = lf.img.as_raw();
    let sample = |px: u32, py: u32| -> Color {
        let lx = ((px as f32 - lf.ox as f32) / lf.scale).floor() as i32;
        let ly = ((py as f32 - lf.oy as f32) / lf.scale).floor() as i32;
        let c = if lx >= 0 && ly >= 0 && lx < lw && ly < lh {
            let idx = (ly as usize * lw as usize + lx as usize) * 4;
            [raw[idx], raw[idx + 1], raw[idx + 2]]
        } else {
            [lf.backdrop[0], lf.backdrop[1], lf.backdrop[2]]
        };
        Color::Rgb(c[0], c[1], c[2])
    };
    for cy in 0..area.height {
        let py0 = cy as u32 * fh + fh / 4;
        let py1 = cy as u32 * fh + (fh * 3) / 4;
        for cx in 0..area.width {
            let px = cx as u32 * fw + fw / 2;
            if let Some(cell) = buf.cell_mut((area.left() + cx, area.top() + cy)) {
                cell.set_symbol("▀");
                cell.set_fg(sample(px, py0));
                cell.set_bg(sample(px, py1));
            }
        }
    }
}

fn draw_office(f: &mut Frame, app: &App, scene: &Scene, office: &mut OfficeCtx, area: Rect, occluded: Option<Rect>, now_ms: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let was_prepared = std::mem::take(&mut office.prepared);
    let proto_ty = office.picker.as_ref().map(|p| p.protocol_type());
    if office.view.is_some() && proto_ty.is_some() {
        let proto_ty = proto_ty.unwrap();
        if proto_ty == ProtocolType::Halfblocks {
            let (fw, fh) = office.picker.as_ref().unwrap().font_size();
            let (fw, fh) = (fw.max(1) as u32, fh.max(1) as u32);
            let px_w = (area.width as u32 * fw).min(MAX_PX_W);
            let px_h = (area.height as u32 * fh).min(MAX_PX_H);
            let sel = app.selected.clone();
            if let Some(v) = office.view.as_mut() {
                v.set_hover(app.hover.clone());
                if let Some(lf) = v.compose_logical(scene, sel.as_ref(), px_w, px_h, now_ms) {
                    draw_halfblocks_direct(f.buffer_mut(), area, &lf, fw, fh);
                    office.last_walking = lf.is_walking;
                    office.error = None;
                    return;
                }
                // Fallback for FakeView in tests:
                match guarded(|| v.compose(scene, sel.as_ref(), px_w, px_h, now_ms)) {
                    Ok(img) => {
                        let cells = Rect::new(area.x, area.y, (px_w / fw) as u16, (px_h / fh) as u16);
                        let dynimg = image::DynamicImage::ImageRgba8(img);
                        if let Ok(proto) = office.picker.as_ref().unwrap().new_protocol(dynimg, cells, Resize::Fit(None)) {
                            office.error = None;
                            f.render_widget(Image::new(&proto), area);
                            return;
                        }
                    }
                    Err(e) => office.error = Some(e),
                }
            }
        } else {
            // Prepare if not yet prepared for this state (e.g. unit tests calling draw directly).
            if !was_prepared {
                office.prepare_frame(app, scene, area, now_ms);
            }
            office.prepared = false;
            if proto_ty == ProtocolType::Kitty {
                if let Some(pending) = office.pending_kitty_bytes.take() {
                    // Direct draw fallback (tests without event_loop flush): keep working via stdout if needed,
                    // though live event_loop drains pending_kitty_bytes inside DEC 2026 before term.draw.
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), &pending);
                }
                if office.active_kitty_id != 0 {
                    kitty::render_placeholders(area, f.buffer_mut(), office.active_kitty_id, occluded);
                    return;
                }
            } else {
                // Iterm2 / Sixel: hide image behind open centered modals to prevent terminal z-bleed.
                if occluded.is_some() {
                    draw_text_office(f, app, scene, None, area);
                    return;
                }
                if let Some(proto) = &office.cached_proto {
                    f.render_widget(Image::new(proto), area);
                    return;
                }
            }
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

/// Visual progress bar using `█` and `░` clamped to 0..=100%.
pub fn ctx_bar(pct: f32, width: usize) -> String {
    let pct = pct.clamp(0.0, 100.0);
    let filled = (((pct / 100.0) * width as f32).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn draw_sidebar(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    if area.height < 12 {
        app.layout.alert_rows = Rect::default();
        app.layout.alert_agent_ids.clear();
        draw_agents(f, app, scene, area, now_ms);
    } else if area.height < 18 {
        app.layout.alert_rows = Rect::default();
        app.layout.alert_agent_ids.clear();
        let [agents, router] = Layout::vertical([Constraint::Min(4), Constraint::Length(6)]).areas(area);
        draw_agents(f, app, scene, agents, now_ms);
        draw_router(f, scene, router);
    } else {
        let alerts_h = (area.height / 3).clamp(4, 10);
        let [agents, router, alerts] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(6), Constraint::Length(alerts_h)]).areas(area);
        draw_agents(f, app, scene, agents, now_ms);
        draw_router(f, scene, router);
        draw_alerts(f, app, scene, alerts, now_ms);
    }
}

pub const AGENT_COLS: [u16; 6] = [10, 7, 5, 4, 5, 4];

pub fn agent_cells(a: &Agent, depth: usize, now_ms: u64) -> [String; 6] {
    [
        trunc(&tree_label(&a.label, depth), AGENT_COLS[0] as usize),
        trunc(&role_or_model(a), AGENT_COLS[1] as usize),
        state_short(a.state).to_string(),
        fmt_elapsed_short(now_ms.saturating_sub(a.started_ms)),
        fmt_tokens(a.tokens_shown()),
        fmt_cost_short(a.cost_usd),
    ]
}

fn draw_agents(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    let rows = agent_rows(scene);
    let inner_pre = Block::bordered().inner(area);
    let selected_ag = app.selected_agent(scene);
    let inspector_h: u16 = inspector_height(inner_pre.height, selected_ag.is_some());
    let table_h = inner_pre.height.saturating_sub(inspector_h);
    let cap = table_h.saturating_sub(1) as usize;
    let sel_idx = app.selected.as_ref().and_then(|s| rows.iter().position(|(id, _)| id == s)).unwrap_or(0);
    let offset = if cap > 0 && sel_idx >= cap { sel_idx + 1 - cap } else { 0 };
    let visible: Vec<&(String, usize)> = rows.iter().skip(offset).take(cap).collect();

    let title = if cap > 0 && rows.len() > cap && !visible.is_empty() {
        format!(" Agents ({}) [{}-{}] ", rows.len(), offset + 1, offset + visible.len())
    } else {
        format!(" Agents ({}) ", rows.len())
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        app.layout.row_ids.clear();
        app.layout.agent_rows = Rect::default();
        return;
    }

    let table_area = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(inspector_h));
    let body = Rect::new(table_area.x, table_area.y + 1, table_area.width, table_area.height - 1);
    app.layout.agent_rows = body;
    app.layout.row_ids = visible.iter().map(|(id, _)| id.clone()).collect();

    let compact = inner.width < 38;
    let header = if compact {
        Row::new(["agent", "state", "tok", "$"])
    } else {
        Row::new(["agent", "role", "state", "time", "tok", "$"])
    }
    .style(Style::new().fg(Color::DarkGray).bold());

    let trows: Vec<Row> = visible
        .iter()
        .filter_map(|(id, depth)| scene.agents.get(id).map(|a| (a, *depth)))
        .map(|(a, depth)| {
            let c = agent_cells(a, depth, now_ms);
            let mut st = Style::new().fg(state_color(a.state));
            if a.state == office_core::AgentState::NeedsYou {
                st = st.add_modifier(Modifier::BOLD);
            }
            let row = if compact {
                Row::new(vec![
                    Cell::from(trunc(&tree_label(&a.label, depth), 12)).style(Style::new().fg(kind_color(a.kind))),
                    Cell::from(c[2].clone()).style(st),
                    Cell::from(trunc(&c[4], 6)),
                    Cell::from(trunc(&c[5], 5)),
                ])
            } else {
                Row::new(vec![
                    Cell::from(c[0].clone()).style(Style::new().fg(kind_color(a.kind))),
                    Cell::from(c[1].clone()),
                    Cell::from(c[2].clone()).style(st),
                    Cell::from(c[3].clone()),
                    Cell::from(c[4].clone()),
                    Cell::from(c[5].clone()),
                ])
            };
            if app.selected.as_deref() == Some(a.id.as_str()) {
                row.style(Style::new().bg(Color::Rgb(0x33, 0x3A, 0x4A)).add_modifier(Modifier::BOLD))
            } else {
                row
            }
        })
        .collect();
    if compact {
        let widths = [Constraint::Length(12), Constraint::Length(5), Constraint::Length(6), Constraint::Length(5)];
        f.render_widget(Table::new(trows, widths).header(header).column_spacing(1), table_area);
    } else {
        let widths = AGENT_COLS.map(Constraint::Length);
        f.render_widget(Table::new(trows, widths).header(header).column_spacing(1), table_area);
    }

    if let (true, Some(a)) = (inspector_h >= 2, selected_ag) {
        let insp_area = Rect::new(inner.x, inner.y + table_area.height, inner.width, inspector_h);
        let w = inner.width as usize;
        if inspector_h >= 3 {
            let line1 = Line::from(vec![
                Span::styled("▸ ", Style::new().fg(Color::DarkGray)),
                Span::styled(
                    inspector_header(a, scene, w.saturating_sub(2)),
                    Style::new().fg(kind_color(a.kind)).bold(),
                ),
            ]);
            let line2 = Line::from(Span::styled(
                inspector_state_tool(a, w),
                Style::new().fg(state_color(a.state)),
            ));
            let meta_style = if a.context_pct.is_some_and(|p| p >= 85.0) {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new().fg(Color::DarkGray)
            };
            let line3 = Line::from(Span::styled(inspector_metrics(a, w), meta_style));
            f.render_widget(Paragraph::new(vec![line1, line2, line3]), insp_area);
        } else {
            let act = if a.activity.is_empty() { state_name(a.state) } else { a.activity.as_str() };
            let prefix = "▸ ";
            let label = trunc(&a.label, (inner.width.saturating_sub(4) as usize).min(18));
            let used = prefix.chars().count() + label.chars().count() + 2;
            let rest = trunc(act, (inner.width as usize).saturating_sub(used));
            let line1 = Line::from(vec![
                Span::styled(prefix, Style::new().fg(Color::DarkGray)),
                Span::styled(label, Style::new().fg(kind_color(a.kind)).bold()),
                Span::raw(format!(": {rest}")),
            ]);
            let mut parts = Vec::new();
            if let Some(p) = a.context_pct {
                parts.push(format!("ctx [{}] {p:.0}%", ctx_bar(p, 5)));
            }
            if let Some(r) = a.rss_mb {
                parts.push(format!("{r}M"));
            }
            if let Some(m) = &a.model {
                parts.push(model_short(m));
            } else if let Some(r) = a.role.as_ref().filter(|r| !r.is_empty()) {
                parts.push(r.clone());
            }
            let meta = if parts.is_empty() { "no metrics".to_string() } else { parts.join(" · ") };
            let meta_style = if a.context_pct.is_some_and(|p| p >= 85.0) {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new().fg(Color::DarkGray)
            };
            let line2 = Line::from(Span::styled(trunc(&meta, w), meta_style));
            f.render_widget(Paragraph::new(vec![line1, line2]), insp_area);
        }
    }
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

fn draw_alerts(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    let alerts = app.visible_alerts(scene);
    let title = if app.alerts_cleared_at.is_some() {
        format!(" Alerts ({} · cleared) ", alerts.len())
    } else {
        format!(" Alerts ({}) ", alerts.len())
    };
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    app.layout.alert_rows = inner;
    app.layout.alert_agent_ids = alerts.iter().take(inner.height as usize).map(|a| a.agent.clone()).collect();
    let width = inner.width as usize;
    let items: Vec<ListItem> = alerts
        .iter()
        .map(|al| ListItem::new(alert_line(now_ms, al, scene, width)).style(Style::new().fg(level_color(al.level))))
        .collect();
    f.render_widget(List::new(items).block(block), area);
}

// Toast rendering now happens inside draw_footer so it never occludes Kitty graphics.

pub const HELP: &[(&str, &str)] = &[
    ("q / ctrl-c", "quit"),
    ("↑↓ / j k, g G", "select agent (or click a row / an agent)"),
    ("Tab", "jump to next alerting / active agent"),
    ("enter", "agent details (double-click also opens)"),
    ("PgUp / PgDn", "scroll agent details"),
    ("a / d", "approve / deny the pending permission"),
    ("x", "cancel the selected agy call (asks y/n)"),
    ("o", "resume the selected Claude session"),
    ("t", "cycle theme presets"),
    ("T", "theme mixer (per layer)"),
    ("s", "toggle sidebar"),
    ("n / m", "toggle notifications / audio chime"),
    ("c / C", "clear / restore alerts"),
    ("r", "hand-offs: briefs, reports, results (selected agent's, or all)"),
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
        kv(
            "tokens",
            format!(
                "{} in / {} cache-read / {} cache-write / {} out",
                fmt_tokens(a.tokens_in),
                fmt_tokens(a.tokens_cache_read),
                fmt_tokens(a.tokens_cache_write),
                fmt_tokens(a.tokens_out)
            ),
        ),
        kv("cost", fmt_cost(a.cost_usd)),
        kv(
            "context",
            a.context_pct.map(|p| format!("[{}] {p:.0}%", ctx_bar(p, 10))).unwrap_or_else(|| "-".into()),
        ),
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
    let mine = crate::app::state::report_list(scene, Some(a.id.as_str()));
    v.push(Line::raw(""));
    v.push(Line::from(Span::styled(format!("hand-offs ({}) · press r to read them", mine.len()), Style::new().bold())));
    if mine.is_empty() {
        v.push(Line::raw("  (none yet)"));
    }
    for r in mine.iter().take(8) {
        let (tag, _) = report_kind_tag(r.kind);
        let dir = if r.from == a.id { format!("→ {}", r.to_label) } else { format!("← {}", r.from_label) };
        v.push(Line::raw(format!("  {:>4}  {tag}  {dir}  {}", fmt_ago(now_ms, r.at_ms), trunc(&r.title, 40))));
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

fn draw_popup(f: &mut Frame, app: &mut App, scene: &Scene, office: &OfficeCtx, now_ms: u64) {
    let area = f.area();
    let max_h = area.height.saturating_sub(2).max(5);
    match &app.popup {
        Popup::None => {
            app.layout.popup = Rect::default();
            app.layout.detail_max_scroll = 0;
        }
        Popup::Help => {
            app.layout.detail_max_scroll = 0;
            let r = centered(area, 62, (HELP.len() as u16 + 4).min(max_h));
            app.layout.popup = r;
            let inner = popup_frame(f, r, " help ".into());
            let mut lines: Vec<Line> = HELP
                .iter()
                .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:<16}"), Style::new().fg(Color::Yellow)), Span::raw(*d)]))
                .collect();
            lines.push(Line::from(Span::styled("esc / ? to close", Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::Detail(id) => {
            let Some(a) = scene.agents.get(id) else {
                app.layout.popup = Rect::default();
                return;
            };
            let lines = detail_lines(app, a, scene, now_ms);
            let r = centered(area, 72, (lines.len() as u16 + 2).min(max_h));
            app.layout.popup = r;
            f.render_widget(Clear, r);
            let mut b = Block::bordered()
                .borders(Borders::ALL)
                .title(format!(" {} ", a.label))
                .style(Style::new().bg(Color::Rgb(0x16, 0x18, 0x20)));
            if lines.len() + 2 > r.height as usize {
                b = b.title_bottom(Line::from(" PgUp/PgDn scroll · esc close ").right_aligned());
            }
            let inner = b.inner(r);
            let wrapped: usize = lines
                .iter()
                .map(|l| {
                    let w = l.width();
                    (w.max(1) + inner.width.max(1) as usize - 1) / inner.width.max(1) as usize
                })
                .sum();
            app.layout.detail_max_scroll = (wrapped.min(u16::MAX as usize) as u16).saturating_sub(inner.height);
            app.detail_scroll = app.detail_scroll.min(app.layout.detail_max_scroll);
            f.render_widget(b, r);
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((app.detail_scroll, 0)), inner);
        }
        Popup::Reports { sel, only } => {
            let list = crate::app::state::report_list(scene, only.as_deref());
            let n = list.len() as u16;
            let r = centered(area, 100, (n + 4).clamp(6, max_h));
            app.layout.popup = r;
            let who = only.as_ref().map(|id| scene.agents.get(id).map(|a| a.label.clone()).unwrap_or_else(|| id.clone()));
            let title = match &who {
                Some(w) => format!(" hand-offs · {w} (a: all) "),
                None => " hand-offs · all ".to_string(),
            };
            let inner = popup_frame(f, r, title);
            let rows = inner.height.saturating_sub(1) as usize;
            let start = sel.saturating_sub(rows.saturating_sub(1));
            let w = inner.width as usize;
            let mut lines: Vec<Line> = Vec::new();
            for (i, rep) in list.iter().enumerate().skip(start).take(rows) {
                let (tag, c) = report_kind_tag(rep.kind);
                let head = format!("{:>4}  ", fmt_ago(scene.now_ms, rep.at_ms));
                let route = format!("{} → {}  ", trunc(&rep.from_label, 22), trunc(&rep.to_label, 18));
                let rest = w.saturating_sub(head.chars().count() + 8 + route.chars().count());
                let mut style = Style::new();
                if i == *sel {
                    style = style.bg(Color::Rgb(0x2a, 0x30, 0x40));
                }
                lines.push(Line::from(vec![
                    Span::styled(head, style.fg(Color::DarkGray)),
                    Span::styled(format!("{tag}  "), style.fg(c)),
                    Span::styled(route, style),
                    Span::styled(trunc(&rep.title, rest), style.bold()),
                ]));
            }
            lines.push(Line::from(Span::styled("↑↓ select · enter open · a all · esc close", Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::ReportView { id, .. } => {
            let Some(rep) = scene.reports.iter().find(|r| r.id == *id) else {
                app.layout.popup = Rect::default();
                return;
            };
            let lines = report_lines(rep, now_ms);
            let r = centered(area, 100, max_h);
            app.layout.popup = r;
            let b = Block::bordered()
                .borders(Borders::ALL)
                .title(format!(" {} ", trunc(&rep.title, 60)))
                .title_bottom(Line::from(" ↑↓/PgUp/PgDn scroll · esc back · q close ").right_aligned())
                .style(Style::new().bg(Color::Rgb(0x16, 0x18, 0x20)));
            let inner = b.inner(r);
            let wrapped: usize = lines
                .iter()
                .map(|l| (l.width().max(1) + inner.width.max(1) as usize - 1) / inner.width.max(1) as usize)
                .sum();
            app.layout.detail_max_scroll = (wrapped.min(u16::MAX as usize) as u16).saturating_sub(inner.height);
            app.detail_scroll = app.detail_scroll.min(app.layout.detail_max_scroll);
            f.render_widget(Clear, r);
            f.render_widget(b, r);
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((app.detail_scroll, 0)), inner);
        }
        Popup::ConfirmCancel { label, pid, .. } => {
            let text = format!("Cancel {label} (pid {pid})? y/n");
            let r = centered(area, text.chars().count() as u16 + 6, 3);
            app.layout.popup = r;
            let inner = popup_frame(f, r, " confirm ".into());
            f.render_widget(Paragraph::new(text).style(Style::new().fg(Color::LightRed).bold()), inner);
        }
        Popup::Mixer { layer } => {
            let themes = office.view.as_ref().map(|v| v.themes()).unwrap_or_else(|| vec!["procedural".into()]);
            let r = centered(area, 50, (LAYERS.len() as u16 + 5).min(max_h));
            app.layout.popup = r;
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

    #[test]
    fn responsive_header_and_compact_sidebar() {
        let (mut app, scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        // Width 90 -> compact header + 34-col compact sidebar.
        let s = render(&mut app, &scene, &mut office, 90, 30);
        assert!(s.contains("4a (2c/1s/1g)"), "{s}");
        assert!(s.contains("Alerts (2)"), "{s}");
        assert_eq!(app.layout.agent_rows.x, 90 - 34 + 1);
        assert!(!s.contains("role  state"), "expected 4-col compact table:\n{s}");
        // Very short terminal (<12 body rows -> height 13) hides Router/Alerts.
        let short = render(&mut app, &scene, &mut office, 90, 13);
        assert!(!short.contains("Router"));
        assert!(!short.contains("Alerts"));
        assert!(app.layout.alert_agent_ids.is_empty());
    }

    #[test]
    fn selected_inspector_context_bar_and_hit_caches() {
        let (mut app, mut scene) = test_app();
        scene.agents.get_mut("cc:dice").unwrap().context_pct = Some(62.0);
        scene.agents.get_mut("cc:dice").unwrap().rss_mb = Some(140);
        select(&mut app, "cc:dice");
        let mut office = OfficeCtx::new(None, None);
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("▸ cc·DICE"), "{s}");
        assert!(s.contains("reading · Read src/main.rs"), "{s}");
        assert!(s.contains("ctx [███░░] 62% · 140M · opus-4-5"), "{s}");
        assert_eq!(app.layout.alert_agent_ids, vec![Some("cc-sub:dice/1".into()), None]);
        assert_eq!(app.layout.popup, Rect::default());

        app.popup = Popup::Detail("cc:dice".into());
        let popped = render(&mut app, &scene, &mut office, 120, 40);
        assert!(popped.contains("[██████░░░░] 62%"), "{popped}");
        assert!(app.layout.popup.width > 0 && app.layout.popup.height > 0);
    }
}
