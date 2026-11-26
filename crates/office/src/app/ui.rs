//! Drawing: header, office (compressed Kitty / direct halfblocks / ratatui-image), sidebar, footer, popups.
use super::fmt::*;
use super::kitty;
use super::reader;
use super::panels::{self, SideTab};
use super::state::{layer_get, App, Popup, LAYERS};
use super::{guarded, OfficeView};
use crate::render;
use office_core::{model::AgentKind, Agent, AlertLevel, Scene};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Bar, BarChart, BarGroup, Block, Borders, Cell, Clear, Gauge, LineGauge, List, ListItem, ListState, Paragraph, Row,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline, Table, Tabs, Wrap,
    },
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
    last_scanlines: bool,
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
            last_scanlines: false,
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
                && self.last_scanlines == app.show.scanlines
                && self.last_logical == *lf.img.as_raw();
            if unchanged
                && ((proto_ty == ProtocolType::Kitty && self.active_kitty_id != 0)
                    || (proto_ty != ProtocolType::Kitty && self.cached_proto.is_some()))
            {
                self.error = None;
                return;
            }
            self.last_area = cells;
            self.last_px = (px_w, px_h);
            self.last_logical_dims = dims;
            self.last_scanlines = app.show.scanlines;
            self.last_logical.clear();
            self.last_logical.extend_from_slice(lf.img.as_raw());

            if self.scaled_scratch.dimensions() != (px_w, px_h) {
                self.scaled_scratch = image::RgbaImage::new(px_w, px_h);
            }
            for px in self.scaled_scratch.as_mut().as_chunks_mut::<4>().0 {
                px.copy_from_slice(&lf.backdrop);
            }
            render::canvas::scale_into_with_xmap(&mut self.scaled_scratch, lf.img, lf.ox, lf.oy, lf.scale, &mut self.xmap);
            if app.show.scanlines {
                render::apply_scanlines(&mut self.scaled_scratch);
            }

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

fn confirm_cancel_text(scene: &Scene, id: &str, label: &str, pid: u32) -> String {
    match scene.agents.get(id).and_then(|a| a.rss_mb) {
        Some(rss) => format!("Cancel {label} (pid {pid}, ~{rss} MB)? y/n"),
        None => format!("Cancel {label} (pid {pid})? y/n"),
    }
}

fn approve_all_text_and_w(items: &[(String, String, String)]) -> (String, String, u16) {
    let msg = format!("Approve all {} pending permissions? y/n", items.len());
    let names: String = items.iter().map(|(_, l, _)| l.as_str()).collect::<Vec<_>>().join(", ");
    let w = msg.chars().count().max(names.chars().count().min(60)) as u16 + 6;
    (msg, names, w.clamp(42, 72))
}

fn confirm_abort_auto_text(job: &str, phase: &str) -> String {
    if phase.is_empty() {
        format!("Abort auto job {job}?")
    } else {
        format!("Abort auto job {job} (phase {phase})?")
    }
}

pub fn popup_rect(area: Rect, app: &App, scene: &Scene) -> Option<Rect> {
    let max_h = area.height.saturating_sub(2).max(5);
    match &app.popup {
        Popup::None => None,
        Popup::Help => Some(centered(area, 78, (help_lines().len() as u16 + 2).min(max_h))),
        Popup::Detail(id) => scene.agents.get(id).map(|a| {
            let lines = detail_lines(app, a, scene, scene.now_ms);
            centered(area, 72, (lines.len() as u16 + 2).min(max_h))
        }),
        Popup::ConfirmCancel { id, label, pid } => {
            let text = confirm_cancel_text(scene, id, label, *pid);
            Some(centered(area, text.chars().count() as u16 + 6, 3))
        }
        Popup::Theme { names, .. } => Some(centered(area, 56, (names.len() as u16 + 4).min(max_h))),
        Popup::Mixer { .. } => Some(centered(area, 50, (LAYERS.len() as u16 + 5).min(max_h))),
        Popup::View { .. } => Some(centered(area, 68, (office_core::config::ShowConfig::ITEMS.len() as u16 + 8).min(max_h))),
        Popup::Reports { only, inbox, .. } => {
            let n = crate::app::state::report_list_for(scene, only.as_deref(), *inbox).len() as u16;
            Some(centered(area, 100, (n + 4).clamp(6, max_h)))
        }
        Popup::ReportView { .. } => Some(centered(area, 104, max_h)),
        Popup::FreeRam { targets, .. } => {
            let rows = targets.len().max(1) as u16;
            Some(centered(area, 78, (rows + 7).clamp(8, max_h)))
        }
        Popup::Dispatch { .. } => Some(centered(area, 76, 14.min(max_h))),
        Popup::Actions { .. } => {
            let n = super::state::ACTIONS.len() as u16;
            Some(centered(area, 56, (n + 5).min(max_h)))
        }
        Popup::ConfirmApproveAll { items } => {
            let (_, _, w) = approve_all_text_and_w(items);
            Some(centered(area, w, 5.min(max_h)))
        }
        Popup::ConfirmAbortAuto { job, phase } => {
            let msg = confirm_abort_auto_text(job, phase);
            let hint = "Y / Enter abort · N / Esc cancel";
            let w = msg.chars().count().max(hint.chars().count()) as u16 + 6;
            Some(centered(area, w, 4.min(max_h)))
        }
    }
}

fn report_kind_tag(k: office_core::ReportKind) -> (&'static str, Color) {
    match k {
        office_core::ReportKind::Task => ("brief ", Color::Gray),
        office_core::ReportKind::Report => ("report", Color::Yellow),
        office_core::ReportKind::Result => ("result", Color::LightBlue),
    }
}

/// Rect of the boss-inbox notification card inside the office area (None when it does not fit).
pub fn card_rect(office: Rect) -> Option<Rect> {
    if office.width < 24 || office.height < 6 {
        return None;
    }
    let w = 48.min(office.width);
    let h = 6;
    Some(Rect::new(office.x + office.width - w, office.y + office.height - h, w, h))
}

fn draw_card(f: &mut Frame, app: &mut App, scene: &Scene, office: Rect) {
    app.layout.card = Rect::default();
    if app.popup != Popup::None {
        return;
    }
    let (Some(rep), Some(r)) = (app.card_report(scene), card_rect(office)) else {
        return;
    };
    app.layout.card = r;
    let (tag, c) = if rep.title.starts_with('⚠') {
        ("⚠ escalation", Color::LightRed)
    } else if rep.kind == office_core::ReportKind::Result {
        ("result", Color::LightBlue)
    } else {
        ("report", Color::Yellow)
    };
    let pal = ui_palette(app);
    let unread = app.unread_count(scene);
    let b = Block::bordered()
        .border_style(Style::new().fg(c))
        .title(format!(" ✉ boss inbox{} ", if unread > 1 { format!(" · {unread} unread") } else { String::new() }))
        .title_bottom(Line::from(" enter read · esc dismiss · i inbox ").right_aligned())
        .style(Style::new().bg(pal.popup_bg));
    let inner = b.inner(r);
    let w = inner.width as usize;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(tag.to_string(), Style::new().fg(c).bold()),
            Span::styled(format!("  from {}", trunc(&rep.from_label, w.saturating_sub(tag.chars().count() + 8))), Style::new().fg(Color::Gray)),
        ]),
        Line::from(Span::styled(trunc(&rep.title, w), Style::new().bold())),
    ];
    let parsed = reader::parse_body(&rep.body);
    let mut room = 2usize;
    if let Some((st, col)) = reader::status_line(&parsed) {
        lines.push(Line::from(Span::styled(trunc(&st, w), Style::new().fg(col))));
        room -= 1;
    }
    for l in reader::summary_of(&parsed.content).lines().map(reader::first_line).filter(|l| !l.is_empty()).take(room) {
        lines.push(Line::from(Span::styled(trunc(&l, w), Style::new().fg(Color::Gray))));
    }
    f.render_widget(Clear, r);
    f.render_widget(b, r);
    f.render_widget(Paragraph::new(lines), inner);
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
    let card = if app.popup == Popup::None && app.card_report(scene).is_some() { card_rect(office_area) } else { None };
    let occ = popup_rect(f.area(), app, scene).or(card);
    draw_office(f, app, scene, office, office_area, occ, now_ms);
    match side_area {
        Some(s) => draw_sidebar(f, app, scene, s, now_ms),
        None => {
            app.layout.row_ids.clear();
            app.layout.agent_rows = Rect::default();
            app.layout.alert_rows = Rect::default();
            app.layout.alert_agent_ids.clear();
            app.layout.side_tabs = Rect::default();
            app.layout.free_ram_btn = Rect::default();
        }
    }
    draw_card(f, app, scene, office_area);
    draw_popup(f, app, scene, office, now_ms);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPalette {
    pub border: Color,
    pub accent: Color,
    pub badge_fg: Color,
    pub popup_bg: Color,
    pub sel_bg: Color,
}

pub fn ui_palette(app: &App) -> UiPalette {
    let default = UiPalette {
        border: Color::DarkGray,
        accent: Color::Rgb(0xE0, 0x7A, 0x3F),
        badge_fg: Color::Black,
        popup_bg: Color::Rgb(0x16, 0x18, 0x20),
        sel_bg: Color::Rgb(0x2A, 0x30, 0x40),
    };
    if !app.show.theme_chrome {
        return default;
    }
    match app.mix.walls.as_str() {
        "tokyo-night" => UiPalette {
            border: Color::Rgb(0x41, 0x48, 0x68),
            accent: Color::Rgb(0x7A, 0xA2, 0xF7),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x16, 0x16, 0x1E),
            sel_bg: Color::Rgb(0x28, 0x34, 0x57),
        },
        "nord" => UiPalette {
            border: Color::Rgb(0x4C, 0x56, 0x6A),
            accent: Color::Rgb(0x88, 0xC0, 0xD0),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x24, 0x29, 0x33),
            sel_bg: Color::Rgb(0x3B, 0x42, 0x52),
        },
        "midnight" => UiPalette {
            border: Color::Rgb(0x3A, 0x44, 0x59),
            accent: Color::Rgb(0xF5, 0xA6, 0x42),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x11, 0x13, 0x19),
            sel_bg: Color::Rgb(0x25, 0x29, 0x34),
        },
        "cyberpunk" => UiPalette {
            border: Color::Rgb(0xFF, 0x2B, 0xD6),
            accent: Color::Rgb(0x00, 0xE5, 0xFF),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x0B, 0x06, 0x14),
            sel_bg: Color::Rgb(0x24, 0x15, 0x40),
        },
        "retro" | "retro-gameboy" => UiPalette {
            border: Color::Rgb(0x8B, 0xAC, 0x0F),
            accent: Color::Rgb(0x9B, 0xBC, 0x0F),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x0F, 0x38, 0x0F),
            sel_bg: Color::Rgb(0x30, 0x62, 0x30),
        },
        "retro-1bit" => UiPalette {
            border: Color::Rgb(0x2F, 0xC2, 0x4A),
            accent: Color::Rgb(0xB8, 0xFF, 0xC0),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x00, 0x1A, 0x05),
            sel_bg: Color::Rgb(0x0A, 0x4D, 0x1A),
        },
        "retro-amber" => UiPalette {
            border: Color::Rgb(0xC2, 0x7A, 0x00),
            accent: Color::Rgb(0xFF, 0xC3, 0x4D),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x14, 0x0A, 0x00),
            sel_bg: Color::Rgb(0x3A, 0x20, 0x00),
        },
        "cozy" => UiPalette {
            border: Color::Rgb(0x8A, 0x55, 0x30),
            accent: Color::Rgb(0xE7, 0xB3, 0x6A),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x20, 0x15, 0x0E),
            sel_bg: Color::Rgb(0x3B, 0x27, 0x19),
        },
        "modern" => UiPalette {
            border: Color::Rgb(0x6B, 0x6F, 0x78),
            accent: Color::Rgb(0x7F, 0x8F, 0xE6),
            badge_fg: Color::Black,
            popup_bg: Color::Rgb(0x18, 0x1B, 0x22),
            sel_bg: Color::Rgb(0x2A, 0x30, 0x3C),
        },
        _ => default,
    }
}

fn chrome_block<'a>(title: impl Into<Line<'a>>, pal: UiPalette) -> Block<'a> {
    Block::bordered().title(title).border_style(Style::new().fg(pal.border))
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
    let pal = ui_palette(app);
    let badge_st = Style::new().bold().fg(pal.badge_fg).bg(pal.accent);
    let mut spans = if area.width >= 105 {
        vec![
            Span::styled(" ROSSO · agent office ", badge_st),
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
            Span::styled(" ROSSO ", badge_st),
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
    let unread = app.unread_count(scene);
    if unread > 0 {
        spans.insert(1, Span::styled(format!(" ✉ {unread} "), Style::new().fg(Color::Black).bg(Color::Yellow).bold()));
    }
    if app.sound {
        spans.push(Span::styled("  │ sound", Style::new().fg(Color::Cyan)));
    }
    if !app.notifications {
        spans.push(Span::styled("  │ notif off", Style::new().fg(Color::DarkGray)));
    }
    if app.demo {
        spans.push(Span::styled("  │ DEMO", Style::new().fg(Color::Yellow).bold()));
    }
    let left_w: usize = spans.iter().map(|s| s.width()).sum();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let strip = status_strip(app, scene);
    let strip_w: usize = strip.iter().map(|s| s.width()).sum();
    if area.width as usize >= left_w + strip_w + 3 {
        let r = Rect::new(area.x + area.width - strip_w as u16 - 1, area.y, strip_w as u16, 1);
        f.render_widget(Paragraph::new(Line::from(strip)), r);
    }
}

/// The compact ROSSO status strip on the right of the header: "● 3 active · ✉ 2 · $4.12 · router 2/5".
pub fn status_strip(app: &App, scene: &Scene) -> Vec<Span<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let active = panels::active_count(scene);
    let unread = app.unread_count(scene);
    let r = &scene.router;
    let cap = r.capacity.or(r.max_concurrent);
    let router = match cap {
        Some(c) => format!("router {}/{}", r.busy_slots, c),
        None => format!("router {}/?", r.busy_slots),
    };
    let rc = match cap {
        Some(c) if c > 0 && r.busy_slots >= c => Color::LightRed,
        Some(c) if c > 0 && r.busy_slots * 3 >= c * 2 => Color::Yellow,
        _ => Color::Green,
    };
    vec![
        Span::styled("● ", Style::new().fg(if active > 0 { Color::Green } else { Color::DarkGray })),
        Span::styled(format!("{active} active"), Style::new().fg(Color::Gray)),
        Span::styled(" · ", dim),
        Span::styled(format!("✉ {unread}"), Style::new().fg(if unread > 0 { Color::Yellow } else { Color::DarkGray }).bold()),
        Span::styled(" · ", dim),
        Span::styled(fmt_cost(scene.total_cost_usd), Style::new().fg(Color::Gray)),
        Span::styled(" · ", dim),
        Span::styled(router, Style::new().fg(rc)),
    ]
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
        } else if a.pid.is_some() {
            spans.push(Span::styled("  x stop", Style::new().fg(Color::LightRed)));
        }
        if matches!(a.kind, AgentKind::Claude | AgentKind::ClaudeSub) || a.resume_id.as_deref().is_some_and(|s| !s.is_empty()) {
            spans.push(Span::styled("  o resume", Style::new().fg(Color::Cyan)));
        }
    }
    if !scene.reports.is_empty() {
        spans.push(Span::styled("  r reports", dim));
    }
    if scene.reports.iter().any(|r| r.to.is_none()) {
        spans.push(Span::styled("  i inbox", Style::new().fg(Color::Yellow)));
    }
    spans.push(Span::styled("  p task  f reply  K ram  Space menu  ? help", dim));
    spans.push(Span::styled("  1-4 tabs  Tab jump  t theme  [ ] cycle  T mix  v view  z fit  L labels  s side", dim));
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
    if let (Some(v), Some(picker)) = (office.view.as_mut(), office.picker.as_ref()) {
        let proto_ty = picker.protocol_type();
        if proto_ty == ProtocolType::Halfblocks {
            let (fw, fh) = picker.font_size();
            let (fw, fh) = (fw.max(1) as u32, fh.max(1) as u32);
            let px_w = (area.width as u32 * fw).min(MAX_PX_W);
            let px_h = (area.height as u32 * fh).min(MAX_PX_H);
            let sel = app.selected.clone();
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
                    if let Ok(proto) = picker.new_protocol(dynimg, cells, Resize::Fit(None)) {
                        office.error = None;
                        f.render_widget(Image::new(&proto), area);
                        return;
                    }
                }
                Err(e) => office.error = Some(e),
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
    let (bar, content) = if area.height >= 6 {
        let [b, c] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        (b, c)
    } else {
        (Rect::default(), area)
    };
    app.layout.side_tabs = bar;
    if bar.height > 0 {
        draw_tab_bar(f, app, bar);
    }
    match app.side_tab {
        SideTab::Agents => draw_agents_panel(f, app, scene, content, now_ms),
        other => {
            app.layout.row_ids.clear();
            app.layout.agent_rows = Rect::default();
            app.layout.alert_rows = Rect::default();
            app.layout.alert_agent_ids.clear();
            match other {
                SideTab::Jobs => {
                    app.layout.free_ram_btn = Rect::default();
                    draw_jobs(f, app, scene, content, now_ms);
                }
                SideTab::Activity => {
                    app.layout.free_ram_btn = Rect::default();
                    draw_activity(f, app, scene, content, now_ms);
                }
                _ => draw_stats(f, app, scene, content, now_ms),
            }
        }
    }
}

fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let pal = ui_palette(app);
    let titles: Vec<Line> = SideTab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let tabs = Tabs::new(titles)
        .select(app.side_tab.index())
        .padding("", "")
        .divider(Span::styled("│", Style::new().fg(Color::DarkGray)))
        .style(Style::new().fg(Color::Gray))
        .highlight_style(Style::new().fg(pal.badge_fg).bg(pal.accent).bold());
    f.render_widget(tabs, area);
}

fn draw_free_ram_btn(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    if area.width == 0 || area.height == 0 {
        app.layout.free_ram_btn = Rect::default();
        return;
    }
    let cands = super::state::idle_kill_candidates(scene, now_ms);
    let total_mb: u32 = cands.iter().map(|c| c.rss_mb).sum();
    let orphans = scene.router_extra.orphans;
    let text = if !cands.is_empty() && total_mb > 0 {
        format!(" [K] Free RAM (~{total_mb} MB · {} idle) ", cands.len())
    } else if !cands.is_empty() {
        format!(" [K] Free RAM ({} idle) ", cands.len())
    } else if orphans > 0 {
        format!(" [K] Free RAM ({orphans} orphan agy) ")
    } else {
        " [K] Free RAM (0 idle) ".to_string()
    };
    let style = if !cands.is_empty() || orphans > 0 {
        Style::new().bg(Color::Rgb(0x3a, 0x2e, 0x16)).fg(Color::Yellow).bold()
    } else {
        Style::new().bg(Color::Rgb(0x22, 0x25, 0x2c)).fg(Color::DarkGray)
    };
    app.layout.free_ram_btn = area;
    f.render_widget(Paragraph::new(trunc(&text, area.width as usize)).style(style), area);
}

fn draw_agents_panel(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    if area.height < 12 {
        app.layout.alert_rows = Rect::default();
        app.layout.alert_agent_ids.clear();
        app.layout.free_ram_btn = Rect::default();
        draw_agents(f, app, scene, area, now_ms);
    } else if area.height < 18 {
        app.layout.alert_rows = Rect::default();
        app.layout.alert_agent_ids.clear();
        app.layout.free_ram_btn = Rect::default();
        let [agents, router] = Layout::vertical([Constraint::Min(4), Constraint::Length(6)]).areas(area);
        draw_agents(f, app, scene, agents, now_ms);
        draw_router(f, app, scene, router);
    } else if area.height < 20 {
        app.layout.free_ram_btn = Rect::default();
        let alerts_h = (area.height / 3).clamp(4, 10);
        let [agents, router, alerts] =
            Layout::vertical([Constraint::Min(4), Constraint::Length(6), Constraint::Length(alerts_h)]).areas(area);
        draw_agents(f, app, scene, agents, now_ms);
        draw_router(f, app, scene, router);
        draw_alerts(f, app, scene, alerts, now_ms);
    } else {
        let alerts_h = (area.height / 3).clamp(4, 10);
        let [agents, router, ram_btn, alerts] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(alerts_h),
        ])
        .areas(area);
        draw_agents(f, app, scene, agents, now_ms);
        draw_router(f, app, scene, router);
        draw_free_ram_btn(f, app, scene, ram_btn, now_ms);
        draw_alerts(f, app, scene, alerts, now_ms);
    }
}

/// Short chip text for narrow places.
fn short_chip(v: reader::Verdict) -> Span<'static> {
    reader::chip(&trunc(v.label(), 8), v.color())
}

fn draw_jobs(f: &mut Frame, app: &App, scene: &Scene, area: Rect, now_ms: u64) {
    let pal = ui_palette(app);
    let rows = panels::jobs(scene);
    let block = chrome_block(format!(" Jobs ({}) ", rows.len()), pal);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 8 {
        return;
    }
    let dim = Style::new().fg(Color::DarkGray);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("No ROSSO jobs yet. They appear here when agents send ROSSO documents (memos, task cards, critiques...).")
                .style(dim)
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let w = inner.width as usize;
    let row_h = |j: &panels::JobRow| -> u16 {
        3 + u16::from(j.wave.is_some()) + if j.auto.is_some() { 2 } else { 0 }
    };
    let sel = app.jobs_sel.min(rows.len() - 1);
    let mut offset = 0usize;
    let mut span: u16 = rows[..=sel].iter().map(row_h).sum();
    while offset < sel && span > inner.height {
        span = span.saturating_sub(row_h(&rows[offset]));
        offset += 1;
    }
    let mut cur_y = inner.y;
    let max_y = inner.y + inner.height;
    for (idx, j) in rows.iter().enumerate().skip(offset) {
        if cur_y >= max_y {
            break;
        }
        let avail = max_y - cur_y;
        if avail < 2 {
            break;
        }
        let h = row_h(j).min(avail);
        let y = cur_y;
        cur_y += h;
        let selected = idx == sel;
        let bg = if selected { Style::new().bg(Color::Rgb(0x2a, 0x30, 0x40)) } else { Style::new() };
        let block_area = Rect::new(inner.x, y, inner.width, h);
        f.render_widget(Paragraph::new("").style(bg), block_area);
        let mark = if selected { "▶ " } else { "  " };
        let mut right: Vec<Span<'static>> = vec![Span::styled(j.status.label(), Style::new().fg(j.status.color()).bold())];
        if let Some(v) = j.verdict {
            right.push(Span::raw(" "));
            right.push(short_chip(v));
        }
        right.push(Span::styled(format!(" {}", fmt_ago(now_ms, j.last_ms)), dim));
        let right_w: usize = right.iter().map(|s| s.width()).sum();
        let name = trunc(&j.name, w.saturating_sub(2 + right_w + 1));
        let pad = w.saturating_sub(2 + name.chars().count() + right_w);
        let mut l1 = vec![Span::styled(mark, Style::new().fg(Color::Yellow)), Span::styled(name, Style::new().bold()), Span::raw(" ".repeat(pad))];
        l1.extend(right);
        f.render_widget(Paragraph::new(Line::from(l1)).style(bg), Rect::new(inner.x, y, inner.width, 1));
        if h >= 2 {
            let meta = format!("{} · {} doc{} · {}", j.title, j.docs, if j.docs == 1 { "" } else { "s" }, j.flow);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("  {}", trunc(&meta, w.saturating_sub(2))), Style::new().fg(Color::Gray)))).style(bg),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
        }
        if h >= 3 {
            let label = match j.stage {
                Some(s) => format!("{}/{} {}", s + 1, panels::STAGES.len(), panels::STAGES[s]),
                None => match &j.auto {
                    Some(aj) => {
                        let (step, total) = office_core::rosso::auto_phase_progress(&aj.flow, &aj.phase);
                        if total > 0 {
                            format!("{step}/{total} {}", aj.phase)
                        } else {
                            aj.phase.clone()
                        }
                    }
                    None => format!("0/{} no stage yet", panels::STAGES.len()),
                },
            };
            let g = LineGauge::default()
                .ratio(j.ratio.clamp(0.0, 1.0))
                .label(Span::styled(format!("  {label} "), dim))
                .filled_style(Style::new().fg(j.status.color()))
                .unfilled_style(Style::new().fg(Color::Rgb(0x33, 0x36, 0x40)))
                .style(bg);
            f.render_widget(g, Rect::new(inner.x, y + 2, inner.width, 1));
        }
        let mut extra_y = y + 3;
        if extra_y < y + h {
            if let Some(wave) = &j.wave {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let plan_max = w.saturating_sub(18).max(4);
                let plan = trunc(&wave.plan, plan_max);
                let prefix = format!("  wave {plan}: ");
                let mut used = prefix.chars().count();
                spans.push(Span::styled(prefix, dim));

                let done_tok = format!("{}✓", wave.done);
                used += done_tok.chars().count();
                spans.push(Span::styled(done_tok, Style::new().fg(Color::Green)));

                if wave.relay_inflight > 0 {
                    let r_tok = format!(" {}⟳", wave.relay_inflight);
                    if used + r_tok.chars().count() <= w {
                        used += r_tok.chars().count();
                        spans.push(Span::styled(r_tok, Style::new().fg(Color::Cyan)));
                    }
                }
                if wave.pending > 0 {
                    let p_tok = format!(" {}·", wave.pending);
                    if used + p_tok.chars().count() <= w {
                        used += p_tok.chars().count();
                        spans.push(Span::styled(p_tok, dim));
                    }
                }
                for c in &wave.calls {
                    let Some(raw) = c.verdict.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    let (tok, col) = match reader::Verdict::from_word(&raw.to_ascii_uppercase(), false) {
                        Some(v) => (trunc(v.label(), 8), v.color()),
                        None => (trunc(raw, 8), Color::Gray),
                    };
                    if used + 1 + tok.chars().count() > w {
                        break;
                    }
                    used += 1 + tok.chars().count();
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(tok, Style::new().fg(col).bold()));
                }
                f.render_widget(Paragraph::new(Line::from(spans)).style(bg), Rect::new(inner.x, extra_y, inner.width, 1));
                extra_y += 1;
            }
        }
        if extra_y < y + h {
            if let Some(aj) = &j.auto {
                let (step, total) = office_core::rosso::auto_phase_progress(&aj.flow, &aj.phase);
                let phase_str = if total > 0 {
                    format!("{} ({step}/{total})", aj.phase)
                } else {
                    aj.phase.clone()
                };
                let rest = format!("{} {phase_str} {}/{}", aj.flow, aj.calls_used, aj.max_calls);
                let status_line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled("AUTO", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(" "),
                    Span::styled(trunc(&rest, w.saturating_sub(7)), Style::new().fg(Color::Gray)),
                ]);
                f.render_widget(Paragraph::new(status_line).style(bg), Rect::new(inner.x, extra_y, inner.width, 1));
                extra_y += 1;

                if extra_y < y + h {
                    let hint = "[R]esume / [X]abort";
                    let hint_w = hint.chars().count();
                    let detail_budget = w.saturating_sub(2 + 3 + hint_w);
                    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
                    if let Some(err) = aj.last_error.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        let txt = trunc(&format!("err: {err}"), detail_budget);
                        spans.push(Span::styled(txt, Style::new().fg(Color::LightRed)));
                        spans.push(Span::styled(" · ", dim));
                    } else if let Some(ruling) = aj.ruling.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        let col = reader::Verdict::from_word(&ruling.to_ascii_uppercase(), false)
                            .map(|v| v.color())
                            .unwrap_or(Color::Yellow);
                        let txt = trunc(ruling, detail_budget);
                        spans.push(Span::styled(txt, Style::new().fg(col).bold()));
                        spans.push(Span::styled(" · ", dim));
                    } else if aj.abort_requested {
                        let txt = trunc("aborting…", detail_budget);
                        spans.push(Span::styled(txt, Style::new().fg(Color::Yellow)));
                        spans.push(Span::styled(" · ", dim));
                    }
                    spans.push(Span::styled(trunc(hint, w.saturating_sub(2)), dim));
                    f.render_widget(Paragraph::new(Line::from(spans)).style(bg), Rect::new(inner.x, extra_y, inner.width, 1));
                }
            }
        }
    }
}

fn draw_activity(f: &mut Frame, app: &App, scene: &Scene, area: Rect, now_ms: u64) {
    let pal = ui_palette(app);
    let items = panels::feed(scene, &app.state_log, panels::FEED_MAX);
    let block = chrome_block(format!(" Activity ({}) ", items.len()), pal);
    let inner = block.inner(area);
    if items.is_empty() {
        f.render_widget(
            Paragraph::new("Nothing yet. Hand-offs and agents needing you show up here.").style(Style::new().fg(Color::DarkGray)).wrap(Wrap { trim: true }).block(block),
            area,
        );
        return;
    }
    let w = inner.width as usize;
    let list: Vec<ListItem> = items
        .iter()
        .map(|it| {
            let icon_w = Span::raw(it.icon).width();
            let budget = w.saturating_sub(5 + icon_w + 1);
            let text = if it.route.is_empty() { it.text.clone() } else { format!("{} · {}", it.route, it.text) };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:>4} ", fmt_ago(now_ms, it.at_ms)), Style::new().fg(Color::DarkGray)),
                Span::styled(it.icon, Style::new().fg(it.color)),
                Span::raw(" "),
                Span::styled(trunc(&text, budget), Style::new().fg(Color::Gray)),
            ]))
        })
        .collect();
    let mut st = ListState::default().with_selected(Some(app.feed_sel.min(items.len() - 1)));
    let l = List::new(list).block(block).highlight_style(Style::new().bg(Color::Rgb(0x2a, 0x30, 0x40)).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(l, area, &mut st);
}

fn draw_stats(f: &mut Frame, app: &mut App, scene: &Scene, area: Rect, now_ms: u64) {
    let pal = ui_palette(app);
    let block = chrome_block(" Stats ", pal);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width < 10 {
        app.layout.free_ram_btn = Rect::default();
        return;
    }
    let dim = Style::new().fg(Color::DarkGray);
    let h = inner.height as usize;
    let mut cons = vec![Constraint::Length(1); 4];
    let mut used = 4;
    let (mut has_cost, mut has_active, mut has_bars, mut has_ram_btn) = (false, false, false, false);
    if h >= used + 2 {
        cons.push(Constraint::Length(2));
        has_cost = true;
        used += 2;
    }
    if h >= used + 2 {
        cons.push(Constraint::Length(2));
        has_active = true;
        used += 2;
    }
    if h >= used + 4 {
        cons.push(Constraint::Min(4));
        has_bars = true;
        if h >= used + 5 {
            cons.push(Constraint::Length(1));
            has_ram_btn = true;
        }
    }
    let areas = Layout::vertical(cons).split(inner);
    let w = inner.width as usize;

    // Router capacity gauge, coloured by how full it is.
    let (label, ratio) = router_gauge_label(scene);
    let color = if ratio >= 1.0 { Color::LightRed } else if ratio >= 0.67 { Color::Yellow } else { Color::Green };
    f.render_widget(
        Gauge::default().ratio(ratio).label(format!("router {label}")).gauge_style(Style::new().fg(color).bg(Color::Rgb(0x22, 0x22, 0x22))),
        areas[0],
    );
    let (pl, pc) = pressure_label(scene.router.pressure);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("pressure "),
            Span::styled(pl, Style::new().fg(pc).bold()),
            Span::styled(trunc(&format!(" · queued {} · free {}", scene.router.queued, scene.router.free_mb.map(|v| format!("{v} MB")).unwrap_or_else(|| "?".into())), w.saturating_sub(12)), dim),
        ])),
        areas[1],
    );
    let (ai, aw, ac) = panels::alert_counts(scene);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("alerts "),
            Span::styled(format!("info {ai}"), Style::new().fg(level_color(AlertLevel::Info))),
            Span::raw(" · "),
            Span::styled(format!("warn {aw}"), Style::new().fg(level_color(AlertLevel::Warn))),
            Span::raw(" · "),
            Span::styled(format!("crit {ac}"), Style::new().fg(if ac > 0 { Color::LightRed } else { Color::DarkGray })),
        ])),
        areas[2],
    );
    let (tin, tout) = panels::token_totals(scene);
    f.render_widget(Paragraph::new(trunc(&format!("tokens in {} · out {}", fmt_tokens(tin), fmt_tokens(tout)), w)), areas[3]);

    let mut next = 4;
    if has_cost {
        let a = areas[next];
        next += 1;
        let series = app.samples.cost_series(a.width as usize);
        let delta = match (app.samples.cost.front(), app.samples.cost.back()) {
            (Some(a0), Some(b0)) if app.samples.cost.len() > 1 => format!("  +{}", fmt_cost((b0 - a0).max(0.0))),
            _ => "  collecting…".into(),
        };
        let [l, sp] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(a);
        f.render_widget(Paragraph::new(trunc(&format!("cost 30m  now {}{delta}", fmt_cost(scene.total_cost_usd)), w)).style(Style::new().fg(Color::Gray)), l);
        f.render_widget(Sparkline::default().data(&series).style(Style::new().fg(Color::Yellow)), sp);
    }
    if has_active {
        let a = areas[next];
        next += 1;
        let series = app.samples.active_series(a.width as usize);
        let peak = app.samples.active.iter().copied().max().unwrap_or(0);
        let [l, sp] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(a);
        f.render_widget(
            Paragraph::new(trunc(&format!("active agents  now {} · peak {}", panels::active_count(scene), peak), w)).style(Style::new().fg(Color::Gray)),
            l,
        );
        f.render_widget(Sparkline::default().data(&series).max(peak.max(1)).style(Style::new().fg(Color::Cyan)), sp);
    }
    if has_bars {
        let a = areas[next];
        next += 1;
        let [l, chart] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(a);
        f.render_widget(Paragraph::new("cost per session").style(Style::new().fg(Color::Gray)), l);
        let costs = panels::session_costs(scene, (chart.height as usize).min(6));
        let bars: Vec<Bar> = costs
            .iter()
            .map(|(label, c)| {
                Bar::default()
                    .label(Line::from(trunc(label, 10)))
                    .value((c * 100.0).round() as u64)
                    .text_value(format!("${c:.2}"))
                    .style(Style::new().fg(Color::Rgb(0xE0, 0x7A, 0x3F)))
                    .value_style(Style::new().fg(Color::Black).bg(Color::Rgb(0xE0, 0x7A, 0x3F)))
            })
            .collect();
        f.render_widget(
            BarChart::default()
                .direction(Direction::Horizontal)
                .bar_width(1)
                .bar_gap(0)
                .label_style(Style::new().fg(Color::Gray))
                .data(BarGroup::default().bars(&bars)),
            chart,
        );
    }
    if has_ram_btn {
        draw_free_ram_btn(f, app, scene, areas[next], now_ms);
    } else {
        app.layout.free_ram_btn = Rect::default();
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

    let pal = ui_palette(app);
    let title = if cap > 0 && rows.len() > cap && !visible.is_empty() {
        format!(" Agents ({}) [{}-{}] ", rows.len(), offset + 1, offset + visible.len())
    } else {
        format!(" Agents ({}) ", rows.len())
    };
    let block = chrome_block(title, pal);
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
        let dispatch = office_core::rosso::dispatch_mode(scene, &a.id);
        let queue = scene.queue_tickets.get(&a.id);
        let meta_style = if a.context_pct.is_some_and(|p| p >= 85.0) {
            Style::new().fg(Color::Yellow)
        } else {
            Style::new().fg(Color::DarkGray)
        };
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
            let line3 = inspector_metrics_line(inspector_metrics(a, w), meta_style, dispatch, queue, w);
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
            let line2 = inspector_metrics_line(meta, meta_style, dispatch, queue, w);
            f.render_widget(Paragraph::new(vec![line1, line2]), insp_area);
        }
    }
}

fn dispatch_tag(mode: office_core::DispatchMode) -> (&'static str, Color) {
    match mode {
        office_core::DispatchMode::Direct => ("DIRECT", Color::Cyan),
        office_core::DispatchMode::Relay => ("RELAY", Color::Magenta),
        office_core::DispatchMode::Hybrid => ("DIR+RLY", Color::Yellow),
    }
}

fn queue_label(qt: &office_core::QueueTicket) -> (String, Option<Color>) {
    if qt.repo_busy {
        (format!("queue #{} (repo busy)", qt.pos), Some(Color::Rgb(0xFF, 0x8C, 0x1A)))
    } else {
        (format!("queue #{}", qt.pos), None)
    }
}

fn inspector_metrics_line(
    base: String,
    meta_style: Style,
    dispatch: Option<office_core::DispatchMode>,
    queue: Option<&office_core::QueueTicket>,
    w: usize,
) -> Line<'static> {
    if dispatch.is_none() && queue.is_none() {
        return Line::from(Span::styled(trunc(&base, w), meta_style));
    }
    let mut extras: Vec<(String, Style)> = Vec::new();
    if let Some(mode) = dispatch {
        let (tag, col) = dispatch_tag(mode);
        extras.push((tag.to_string(), Style::new().fg(col).bold()));
    }
    if let Some(qt) = queue {
        let (txt, col) = queue_label(qt);
        let st = col.map(|c| Style::new().fg(c).bold()).unwrap_or(meta_style);
        extras.push((txt, st));
    }
    let extra_chars: usize = extras.iter().map(|(s, _)| s.chars().count()).sum::<usize>()
        + extras.len().saturating_sub(1) * 3;
    let has_base = !base.is_empty() && base != "no metrics";
    let mut spans: Vec<Span<'static>> = Vec::new();
    if has_base {
        let budget = w.saturating_sub(extra_chars + 3);
        if budget > 0 {
            spans.push(Span::styled(trunc(&base, budget), meta_style));
            spans.push(Span::styled(" · ", meta_style));
        }
    }
    let mut used: usize = spans.iter().map(|s| s.width()).sum();
    for (i, (txt, st)) in extras.into_iter().enumerate() {
        if i > 0 {
            if used + 3 >= w {
                break;
            }
            spans.push(Span::styled(" · ", meta_style));
            used += 3;
        }
        let piece = trunc(&txt, w.saturating_sub(used));
        if piece.is_empty() {
            break;
        }
        used += piece.chars().count();
        spans.push(Span::styled(piece, st));
    }
    Line::from(spans)
}

/// Router text lines (without the gauge), for the sidebar and tests.
pub fn router_lines(scene: &Scene, width: usize) -> Vec<Line<'static>> {
    let r = &scene.router;
    let free = r.free_mb.map(|v| format!("{v} MB")).unwrap_or_else(|| "?".into());
    let per = r.per_proc_mb.map(|v| format!("{v} MB")).unwrap_or_else(|| "?".into());
    let rsv = scene
        .router_extra
        .reserved_mb
        .filter(|&v| v > 0)
        .map(|v| format!(" · rsv {v}M"))
        .unwrap_or_default();
    let (pl, pc) = pressure_label(r.pressure);
    let limit = r.limit_text.clone().unwrap_or_else(|| "none".into());
    let base_w = "pressure ".len() + pl.len();
    let q_txt = format!(" · queued {}", r.queued);
    let mut p_spans = vec![
        Span::raw("pressure "),
        Span::styled(pl, Style::new().fg(pc).bold()),
        Span::raw(trunc(&q_txt, width.saturating_sub(base_w))),
    ];
    if scene.router_extra.orphans > 0 {
        let rem = width.saturating_sub(base_w + q_txt.chars().count());
        if rem > 0 {
            let orph = format!(" · {} orphan", scene.router_extra.orphans);
            p_spans.push(Span::styled(trunc(&orph, rem), Style::new().fg(Color::Yellow)));
        }
    }
    vec![
        Line::raw(trunc(&format!("free RAM {free} · per agy {per}{rsv}"), width)),
        Line::from(p_spans),
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

fn draw_router(f: &mut Frame, app: &App, scene: &Scene, area: Rect) {
    let block = chrome_block(" Router ", ui_palette(app));
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
    let block = chrome_block(title, ui_palette(app));
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

/// Help sections: (title, [(keys, what it does)]).
pub const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Moving around",
        &[
            ("↑ ↓ / j k", "pick an agent (or click one); g / G jump to first / last"),
            ("Tab", "jump to the next agent that needs attention"),
            ("enter", "open the selected agent's details (PgUp/PgDn scrolls)"),
            ("1-4", "sidebar tabs: Agents, Jobs, Activity, Stats"),
        ],
    ),
    (
        "Reading work",
        &[
            ("r / i", "reports (hand-offs) / inbox addressed to you"),
            ("enter", "on the inbox card or in a list: read it in full"),
            ("tab ← →", "in a report: Summary / Full / Brief / Raw"),
            ("y / w", "in a report: copy the tab / save it as Markdown"),
        ],
    ),
    (
        "Command Center & actions",
        &[
            ("Space", "open the Command Center quick-actions menu"),
            ("p", "dispatch a new task (agy role / claude)"),
            ("f", "reply / follow-up to the selected agent (or in a report)"),
            ("K", "Free RAM: stop idle agents or reap orphan agy"),
            ("a / d / A", "approve / deny permission, or A to approve all pending"),
            ("x", "stop / cancel the selected agent process (asks y/n first)"),
            ("R / X", "on Jobs tab: resume / abort selected rosso-auto job"),
            ("o / g", "resume session in a terminal / in a report: go to sender"),
        ],
    ),
    (
        "Display",
        &[
            ("v", "view menu: toggles, c clean / a all / h hide"),
            ("z", "toggle pixel-exact vs fill viewport scaling"),
            ("L", "show / hide name tags and speech bubbles"),
            ("t / [ ]", "theme picker (swatches + live preview) / cycle"),
            ("T", "theme mixer: a different theme per layer"),
            ("s", "show / hide the sidebar"),
        ],
    ),
    (
        "Alerts & quit",
        &[
            ("n / m", "desktop notifications / sound chimes on or off"),
            ("c / C", "clear alerts / bring them back"),
            ("q / ctrl-c", "leave the office (agents keep working)"),
        ],
    ),
];

/// The help popup text, section by section.
pub fn help_lines() -> Vec<Line<'static>> {
    let mut v: Vec<Line<'static>> = Vec::new();
    for (i, (title, rows)) in HELP.iter().enumerate() {
        if i > 0 {
            v.push(Line::raw(""));
        }
        v.push(Line::from(Span::styled(title.to_string(), Style::new().fg(Color::LightCyan).bold())));
        for (k, d) in rows.iter() {
            v.push(Line::from(vec![Span::styled(format!("  {k:<11}"), Style::new().fg(Color::Yellow)), Span::raw(d.to_string())]));
        }
    }
    v.push(Line::raw(""));
    v.push(Line::from(Span::styled("full guide: docs/GUIDE.md · esc or ? closes", Style::new().fg(Color::DarkGray))));
    v
}

/// What each View menu row means (label, one-line explanation), in `ShowConfig::ITEMS` order.
pub const VIEW_INFO: [(&str, &str); 16] = [
    ("names", "the name tag above each agent"),
    ("bubbles", "what each agent is doing right now (speech bubble)"),
    ("icons", "small !, ?, … and ✓ above heads"),
    ("nameplates", "the session name on each coloured floor"),
    ("zones", "the coloured floor under each session"),
    ("couriers", "postmen carrying briefs and reports"),
    ("boss visits", "sessions walking to your office with results or plans"),
    ("cat", "the office cat wandering the aisles"),
    ("CEO corner", "your corner office (you are the CEO)"),
    ("council cabin", "meeting room for council members"),
    ("ambient", "coffee steam, server lights, kanban, strolls"),
    ("selected only", "show tags and bubbles only for the selected agent and alerts"),
    ("lighting", "window light shafts, monitor glow halos & depth"),
    ("fill viewport", "scale office to fill pane instead of integer-only steps"),
    ("scanlines", "subtle CRT scanline pass over the office output"),
    ("theme chrome", "match sidebar & popup borders and accents to active theme"),
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
    ];
    if let Some(mode) = office_core::rosso::dispatch_mode(scene, &a.id) {
        let (tag, col) = dispatch_tag(mode);
        v.push(Line::from(vec![
            Span::styled(format!("{:<12}", "dispatch"), Style::new().fg(Color::DarkGray)),
            Span::styled(tag, Style::new().fg(col).bold()),
        ]));
    }
    if let Some(qt) = scene.queue_tickets.get(&a.id) {
        let (txt, col) = queue_label(qt);
        let st = col.map(|c| Style::new().fg(c).bold()).unwrap_or_default();
        v.push(Line::from(vec![
            Span::styled(format!("{:<12}", "queue"), Style::new().fg(Color::DarkGray)),
            Span::styled(txt, st),
        ]));
    }
    v.push(Line::raw(""));
    v.push(Line::from(Span::styled("recent activity", Style::new().bold())));
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

/// One-line blurb for a built-in theme preset (generic label for user themes).
fn theme_desc(name: &str) -> &'static str {
    match name {
        "procedural" => "built-in pixels",
        "modern" => "bright and clean",
        "cozy" => "warm wood",
        "midnight" => "dark blue night",
        "tokyo-night" => "indigo dark studio",
        "nord" => "arctic slate frost",
        "retro" => "8-bit handheld",
        "retro-1bit" => "green phosphor",
        "retro-amber" => "amber CRT",
        "retro-gameboy" => "4-shade pocket",
        "cyberpunk" => "neon synth grid",
        "custom mix…" => "",
        _ => "custom theme",
    }
}

fn theme_swatches(name: &str) -> Option<[Color; 4]> {
    let s = match name {
        "procedural" => [Color::Rgb(0x8F, 0xA3, 0xB8), Color::Rgb(0xE8, 0xE2, 0xD6), Color::Rgb(0x3F, 0x8F, 0x8A), Color::Rgb(0x7D, 0x5A, 0x8C)],
        "modern" => [Color::Rgb(0x5F, 0x66, 0x72), Color::Rgb(0xC8, 0x94, 0x5E), Color::Rgb(0x7F, 0x8F, 0xE6), Color::Rgb(0xC0, 0x60, 0x3A)],
        "cozy" => [Color::Rgb(0x8A, 0x55, 0x30), Color::Rgb(0xC8, 0x94, 0x5E), Color::Rgb(0xA8, 0x43, 0x2F), Color::Rgb(0xE7, 0xB3, 0x6A)],
        "midnight" => [Color::Rgb(0x1C, 0x22, 0x30), Color::Rgb(0x4D, 0x37, 0x28), Color::Rgb(0xF5, 0xA6, 0x42), Color::Rgb(0x6B, 0x55, 0x8A)],
        "tokyo-night" => [Color::Rgb(0x1F, 0x23, 0x35), Color::Rgb(0x34, 0x3B, 0x58), Color::Rgb(0x7A, 0xA2, 0xF7), Color::Rgb(0x73, 0xDA, 0xCA)],
        "nord" => [Color::Rgb(0x3B, 0x42, 0x52), Color::Rgb(0xD8, 0xDE, 0xE9), Color::Rgb(0x88, 0xC0, 0xD0), Color::Rgb(0xA3, 0xBE, 0x8C)],
        "retro" => [Color::Rgb(0x0F, 0x38, 0x0F), Color::Rgb(0x20, 0x40, 0x20), Color::Rgb(0x5A, 0x84, 0x18), Color::Rgb(0x9B, 0xBC, 0x0F)],
        "retro-1bit" => [Color::Rgb(0x00, 0x1A, 0x05), Color::Rgb(0x0A, 0x4D, 0x1A), Color::Rgb(0x2F, 0xC2, 0x4A), Color::Rgb(0xB8, 0xFF, 0xC0)],
        "retro-amber" => [Color::Rgb(0x14, 0x0A, 0x00), Color::Rgb(0x5C, 0x33, 0x00), Color::Rgb(0xC2, 0x7A, 0x00), Color::Rgb(0xFF, 0xC3, 0x4D)],
        "retro-gameboy" => [Color::Rgb(0x0F, 0x38, 0x0F), Color::Rgb(0x30, 0x62, 0x30), Color::Rgb(0x8B, 0xAC, 0x0F), Color::Rgb(0x9B, 0xBC, 0x0F)],
        "cyberpunk" => [Color::Rgb(0x13, 0x0B, 0x24), Color::Rgb(0xFF, 0x2B, 0xD6), Color::Rgb(0x00, 0xE5, 0xFF), Color::Rgb(0x2E, 0xE6, 0x8A)],
        _ => return None,
    };
    Some(s)
}

fn popup_frame_pal(f: &mut Frame, r: Rect, title: String, pal: UiPalette) -> Rect {
    f.render_widget(Clear, r);
    let b = Block::bordered()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(pal.border))
        .title(title)
        .style(Style::new().bg(pal.popup_bg));
    let inner = b.inner(r);
    f.render_widget(b, r);
    inner
}

fn draw_popup(f: &mut Frame, app: &mut App, scene: &Scene, office: &OfficeCtx, now_ms: u64) {
    let area = f.area();
    let max_h = area.height.saturating_sub(2).max(5);
    let pal = ui_palette(app);
    match &app.popup {
        Popup::None => {
            app.layout.popup = Rect::default();
            app.layout.detail_max_scroll = 0;
        }
        Popup::Help => {
            let lines = help_lines();
            let r = centered(area, 78, (lines.len() as u16 + 2).min(max_h));
            app.layout.popup = r;
            f.render_widget(Clear, r);
            let mut b = Block::bordered()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(pal.border))
                .title(" help ")
                .style(Style::new().bg(pal.popup_bg));
            if lines.len() + 2 > r.height as usize {
                b = b.title_bottom(Line::from(" ↑↓ / PgUp PgDn scroll · esc close ").right_aligned());
            }
            let inner = b.inner(r);
            app.layout.detail_max_scroll = (lines.len().min(u16::MAX as usize) as u16).saturating_sub(inner.height);
            app.detail_scroll = app.detail_scroll.min(app.layout.detail_max_scroll);
            f.render_widget(b, r);
            f.render_widget(Paragraph::new(lines).scroll((app.detail_scroll, 0)), inner);
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
                .border_style(Style::new().fg(pal.border))
                .title(format!(" {} ", a.label))
                .style(Style::new().bg(pal.popup_bg));
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
        Popup::Reports { sel, only, inbox } => {
            let list = crate::app::state::report_list_for(scene, only.as_deref(), *inbox);
            let n = list.len() as u16;
            let r = centered(area, 100, (n + 4).clamp(6, max_h));
            app.layout.popup = r;
            let who = only.as_ref().map(|id| scene.agents.get(id).map(|a| a.label.clone()).unwrap_or_else(|| id.clone()));
            let title = if *inbox {
                format!(" inbox · {} unread (a: all hand-offs) ", app.unread_count(scene))
            } else {
                match &who {
                Some(w) => format!(" hand-offs · {w} (a: all) "),
                None => " hand-offs · all ".to_string(),
                }
            };
            let inner = popup_frame_pal(f, r, title, pal);
            let rows = inner.height.saturating_sub(1) as usize;
            let start = sel.saturating_sub(rows.saturating_sub(1));
            let w = inner.width as usize;
            let mut lines: Vec<Line> = Vec::new();
            for (i, rep) in list.iter().enumerate().skip(start).take(rows) {
                let (tag, c) = report_kind_tag(rep.kind);
                let head = format!("{:>4}  ", fmt_ago(scene.now_ms, rep.at_ms));
                let route = format!("{} → {}  ", trunc(&rep.from_label, 22), trunc(&rep.to_label, 18));
                let rest = w.saturating_sub(head.chars().count() + 10 + route.chars().count());
                let unread = *inbox && app.inbox_seen < rep.id;
                let mut style = Style::new();
                if i == *sel {
                    style = style.bg(pal.sel_bg);
                }
                let route_style = if unread { style.bold() } else { style };
                lines.push(Line::from(vec![
                    Span::styled(if unread { "• " } else { "  " }, style.fg(Color::Yellow).bold()),
                    Span::styled(head, style.fg(Color::DarkGray)),
                    Span::styled(format!("{tag}  "), style.fg(c)),
                    Span::styled(route, route_style),
                    Span::styled(trunc(&rep.title, rest), style.bold()),
                ]));
            }
            lines.push(Line::from(Span::styled("↑↓ select · enter open · a all · i inbox · esc close", Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::ReportView { id, only, inbox } => {
            let Some(rep) = scene.reports.iter().find(|r| r.id == *id) else {
                app.layout.popup = Rect::default();
                return;
            };
            let r = centered(area, 104, max_h);
            app.layout.popup = r;
            let list = crate::app::state::report_list_for(scene, only.as_deref(), *inbox);
            let pos = list.iter().position(|x| x.id == *id).map(|i| format!(" {}/{} ", i + 1, list.len())).unwrap_or_default();
            let b = Block::bordered()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(pal.border))
                .title(Line::from(vec![Span::styled(" report", Style::new().bold()), Span::styled(pos, Style::new().fg(Color::Yellow).bold())]))
                .title_bottom(Line::from(" tab/←→ view · ↑↓ scroll · y copy · w save · n/p next/prev · f reply · g sender · esc back ").right_aligned())
                .style(Style::new().bg(pal.popup_bg));
            let inner = b.inner(r);
            f.render_widget(Clear, r);
            f.render_widget(b, r);
            let card = reader::header_card(rep, scene, now_ms, (inner.width as usize).saturating_sub(2));
            let card_h = (card.len() as u16).min(inner.height.saturating_sub(2));
            let [card_a, tabs_a, body_a] =
                Layout::vertical([Constraint::Length(card_h), Constraint::Length(1), Constraint::Min(0)]).areas(inner);
            let pad = |a: Rect| Rect::new(a.x + 1, a.y, a.width.saturating_sub(2), a.height);
            f.render_widget(Paragraph::new(card), pad(card_a));
            let titles: Vec<Line> = reader::ReaderTab::ALL.iter().map(|t| Line::from(t.label())).collect();
            f.render_widget(
                Tabs::new(titles)
                    .select(app.reader_tab.index())
                    .padding(" ", " ")
                    .divider(Span::styled("│", Style::new().fg(Color::DarkGray)))
                    .style(Style::new().fg(Color::Gray))
                    .highlight_style(Style::new().fg(Color::Black).bg(Color::Yellow).bold()),
                tabs_a,
            );
            let [text_a, bar_a] = Layout::horizontal([Constraint::Min(1), Constraint::Length(1)]).areas(body_a);
            let text_w = (text_a.width as usize).saturating_sub(4);
            let mut lines = reader::tab_lines(rep, app.reader_tab, text_w);
            let margin = (text_a.width as usize).saturating_sub(text_w) / 2;
            for l in lines.iter_mut() {
                l.spans.insert(0, Span::raw(" ".repeat(margin)));
            }
            let total = lines.len();
            app.layout.detail_max_scroll = (total.min(u16::MAX as usize) as u16).saturating_sub(text_a.height);
            app.detail_scroll = app.detail_scroll.min(app.layout.detail_max_scroll);
            f.render_widget(Paragraph::new(lines).scroll((app.detail_scroll, 0)), text_a);
            if total > text_a.height as usize {
                let mut sb = ScrollbarState::new(total).viewport_content_length(text_a.height as usize).position(app.detail_scroll as usize);
                f.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::new().fg(Color::DarkGray)).begin_symbol(None).end_symbol(None),
                    bar_a,
                    &mut sb,
                );
            }
        }
        Popup::ConfirmCancel { id, label, pid } => {
            let text = confirm_cancel_text(scene, id, label, *pid);
            let r = centered(area, text.chars().count() as u16 + 6, 3);
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " confirm ".into(), pal);
            f.render_widget(Paragraph::new(text).style(Style::new().fg(Color::LightRed).bold()), inner);
        }
        Popup::View { sel } => {
            let items = office_core::config::ShowConfig::ITEMS;
            let r = centered(area, 68, (items.len() as u16 + 8).min(max_h));
            app.layout.popup = r;
            let pal = ui_palette(app);
            let inner = popup_frame_pal(f, r, " view: what the office shows ".into(), pal);
            let w = inner.width as usize;
            let cap = inner.height.saturating_sub(5).max(1) as usize;
            let start = sel.saturating_sub(cap.saturating_sub(1));
            let mut lines: Vec<Line> = VIEW_INFO
                .iter()
                .enumerate()
                .skip(start)
                .take(cap)
                .map(|(i, (label, _))| {
                    let style = if i == *sel { Style::new().fg(Color::Yellow).bold() } else { Style::new() };
                    let mark = if i == *sel { "▶" } else { " " };
                    let b = if app.show.get(i) { "[x]" } else { "[ ]" };
                    Line::from(Span::styled(format!("{mark} {b} {label}"), style))
                })
                .collect();
            lines.push(Line::raw(""));
            let desc = VIEW_INFO.get(*sel).map(|(l, d)| format!("{l} — {d}")).unwrap_or_default();
            lines.push(Line::from(Span::styled(trunc(&desc, w), Style::new().fg(Color::LightCyan))));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                trunc("↑↓ move  space toggle  a all  c clean  h hide  esc save", w),
                Style::new().fg(Color::DarkGray),
            )));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        Popup::Theme { sel, names, .. } => {
            let r = centered(area, 56, (names.len() as u16 + 4).min(max_h));
            app.layout.popup = r;
            let pal = ui_palette(app);
            let inner = popup_frame_pal(f, r, " theme ".into(), pal);
            let cur = names.iter().position(|n| crate::app::state::mix_eq(&app.preset_mix(n), &app.mix)).unwrap_or(names.len());
            let w = inner.width as usize;
            let cap = inner.height.saturating_sub(1).max(1) as usize;
            let start = sel.saturating_sub(cap.saturating_sub(1));
            let mut lines: Vec<Line> = names
                .iter()
                .map(String::as_str)
                .chain(std::iter::once("custom mix…"))
                .enumerate()
                .skip(start)
                .take(cap)
                .map(|(i, n)| {
                    let style = if i == *sel { Style::new().fg(Color::Yellow).bold() } else { Style::new() };
                    let mark = if i == *sel { "▶" } else { " " };
                    let dot = if i == cur { "●" } else { " " };
                    let d = theme_desc(n);
                    let num = format!("{:<2}", i + 1);
                    let body = if d.is_empty() {
                        format!("{mark}{dot}{} {n}", num.trim_end())
                    } else {
                        format!("{mark}{dot}{} {n:<13} {d}", num.trim_end())
                    };
                    if let Some(sw) = theme_swatches(n) {
                        let sw_w = 5;
                        let trimmed = trunc(&body, w.saturating_sub(sw_w + 1));
                        let used = trimmed.chars().count();
                        let pad = w.saturating_sub(used + sw_w).max(1);
                        let mut spans = vec![
                            Span::styled(trimmed, style),
                            Span::raw(" ".repeat(pad)),
                        ];
                        for c in sw {
                            spans.push(Span::styled("█", Style::new().fg(c)));
                        }
                        Line::from(spans)
                    } else {
                        Line::from(Span::styled(trunc(&body, w), style))
                    }
                })
                .collect();
            lines.push(Line::from(Span::styled(trunc("↑↓ preview  1-9 jump  ⏎ keep  esc undo", w), Style::new().fg(Color::DarkGray))));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::Mixer { layer } => {
            let themes = office.view.as_ref().map(|v| v.themes()).unwrap_or_else(|| vec!["procedural".into()]);
            let r = centered(area, 50, (LAYERS.len() as u16 + 5).min(max_h));
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " theme mixer ".into(), pal);
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
        Popup::FreeRam { sel, targets, checked } => {
            let rows = targets.len().max(1) as u16;
            let r = centered(area, 78, (rows + 7).clamp(8, max_h));
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " Free RAM — kill idle agents ".into(), pal);
            let total_mb: u32 = targets.iter().zip(checked.iter()).filter(|(_, c)| **c).map(|(t, _)| t.rss_mb).sum();
            let checked_n = checked.iter().filter(|&&c| c).count();
            let orphans = scene.router_extra.orphans;
            let [hdr_a, list_a, hint_a] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).areas(inner);
            let w = inner.width as usize;
            let summary = format!("Reclaimable: ~{total_mb} MB ({checked_n} selected) · or press r to reap {orphans} orphans");
            f.render_widget(
                Paragraph::new(trunc(&summary, w)).style(Style::new().fg(Color::LightCyan).bold()),
                hdr_a,
            );
            let list_lines: Vec<Line> = if targets.is_empty() {
                vec![Line::from(Span::styled("  No idle agents with live PIDs.", Style::new().fg(Color::DarkGray)))]
            } else {
                let cap = list_a.height as usize;
                let start = sel.saturating_sub(cap.saturating_sub(1));
                targets
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(cap)
                    .map(|(i, t)| {
                        let chk = if *checked.get(i).unwrap_or(&false) { "[x]" } else { "[ ]" };
                        let mark = if i == *sel { "▶" } else { " " };
                        let badge = if t.is_orphan {
                            "orphan".to_string()
                        } else {
                            scene
                                .agents
                                .get(&t.id)
                                .and_then(|a| a.role.as_deref().filter(|r| !r.is_empty()).map(str::to_string).or_else(|| {
                                    Some(match a.kind {
                                        AgentKind::Claude => "claude".into(),
                                        AgentKind::ClaudeSub => "sub".into(),
                                        AgentKind::Agy => "agy".into(),
                                        AgentKind::Other => "agent".into(),
                                    })
                                }))
                                .unwrap_or_default()
                        };
                        let idle = fmt_elapsed_short(t.idle_ms);
                        let rss = if t.rss_mb > 0 { format!("~{} MB", t.rss_mb) } else { "-".into() };
                        let label_w = w.saturating_sub(30).max(8);
                        let line_txt = format!(
                            "{mark} {chk} {:<label_w$}  {:<10}  {:>4}  {:>7}",
                            trunc(&t.label, label_w),
                            trunc(&badge, 10),
                            idle,
                            rss
                        );
                        let st = if i == *sel {
                            Style::new().bg(Color::Rgb(0x2a, 0x30, 0x40)).fg(Color::Yellow).bold()
                        } else {
                            Style::new()
                        };
                        Line::from(Span::styled(trunc(&line_txt, w), st))
                    })
                    .collect()
            };
            f.render_widget(Paragraph::new(list_lines), list_a);
            f.render_widget(
                Paragraph::new(trunc(" Space toggle · a all · r reap orphans · Enter kill · Esc cancel ", w))
                    .style(Style::new().fg(Color::DarkGray)),
                hint_a,
            );
        }
        Popup::Dispatch { field, target_idx, cwd, prompt, followup } => {
            let r = centered(area, 76, 14.min(max_h));
            app.layout.popup = r;
            let title = followup
                .as_ref()
                .map(|(_, l)| format!(" Reply to {l} "))
                .unwrap_or_else(|| " Dispatch Task ".into());
            let inner = popup_frame_pal(f, r, title, pal);
            let w = inner.width as usize;
            let [top_a, cwd_a, prompt_a, hint_a] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(inner);
            let f0_line = match followup {
                Some((_, label)) => Line::from(Span::styled(trunc(&format!("Reply:   {label}"), w), Style::new().fg(Color::DarkGray))),
                None => {
                    let target_label = super::state::DISPATCH_TARGETS
                        .get(*target_idx)
                        .map(|t| t.1)
                        .unwrap_or("unknown");
                    let st = if *field == 0 { Style::new().fg(Color::Yellow).bold() } else { Style::new() };
                    Line::from(Span::styled(trunc(&format!("Target:  ◀ {target_label} ▶"), w), st))
                }
            };
            f.render_widget(Paragraph::new(f0_line), top_a);

            let cwd_cursor = if *field == 1 { "▌" } else { "" };
            let cwd_st = if *field == 1 { Style::new().fg(Color::Yellow).bold() } else { Style::new() };
            f.render_widget(
                Paragraph::new(Span::styled(trunc(&format!("CWD:     {cwd}{cwd_cursor}"), w), cwd_st)),
                cwd_a,
            );

            let p_border = if *field == 2 { Color::Yellow } else { pal.border };
            let p_block = Block::bordered()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(p_border))
                .style(Style::new().bg(pal.popup_bg));
            let p_cursor = if *field == 2 { "▌" } else { "" };
            let p_st = if *field == 2 { Style::new().fg(Color::Yellow).bold() } else { Style::new().fg(Color::DarkGray) };
            let p_line = Line::from(vec![Span::styled("Prompt:  ", p_st), Span::raw(format!("{prompt}{p_cursor}"))]);
            f.render_widget(Paragraph::new(p_line).block(p_block).wrap(Wrap { trim: false }), prompt_a);

            f.render_widget(
                Paragraph::new(trunc(" Tab field · ←→ target · Enter send · Esc cancel ", w))
                    .style(Style::new().fg(Color::DarkGray)),
                hint_a,
            );
        }
        Popup::Actions { sel } => {
            let n = super::state::ACTIONS.len() as u16;
            let r = centered(area, 56, (n + 5).min(max_h));
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " Command Center ".into(), pal);
            let w = inner.width as usize;
            let mut lines: Vec<Line> = super::state::ACTIONS
                .iter()
                .enumerate()
                .map(|(i, (key, desc))| {
                    let row_txt = format!("  [{key}]  {desc}");
                    let st = if i == *sel {
                        Style::new().bg(pal.sel_bg).fg(Color::Yellow).bold()
                    } else {
                        Style::new()
                    };
                    Line::from(Span::styled(trunc(&row_txt, w), st))
                })
                .collect();
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                trunc(" ↑↓/Enter run · or press shortcut · Esc close ", w),
                Style::new().fg(Color::DarkGray),
            )));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::ConfirmApproveAll { items } => {
            let (msg, names, w) = approve_all_text_and_w(items);
            let r = centered(area, w, 5.min(max_h));
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " confirm approve all ".into(), pal);
            let iw = inner.width as usize;
            let lines = vec![
                Line::from(Span::styled(trunc(&msg, iw), Style::new().fg(Color::Yellow).bold())),
                Line::from(Span::styled(trunc(&names, iw), Style::new().fg(Color::Gray))),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        Popup::ConfirmAbortAuto { job, phase } => {
            let msg = confirm_abort_auto_text(job, phase);
            let hint = "Y / Enter abort · N / Esc cancel";
            let w = msg.chars().count().max(hint.chars().count()) as u16 + 6;
            let r = centered(area, w, 4.min(max_h));
            app.layout.popup = r;
            let inner = popup_frame_pal(f, r, " confirm abort auto ".into(), pal);
            let iw = inner.width as usize;
            let lines = vec![
                Line::from(Span::styled(trunc(&msg, iw), Style::new().fg(Color::LightRed).bold())),
                Line::from(Span::styled(trunc(hint, iw), Style::new().fg(Color::DarkGray))),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;
    use ratatui::{backend::TestBackend, Terminal};
    use crate::app::panels::SideTab;

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
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("a different theme per layer") && render(&mut app, &scene, &mut office, 120, 40).contains("docs/GUIDE.md"));
        app.popup = Popup::Detail("cc:office".into());
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("perm-1") && s.contains("recent activity") && s.contains("/tmp/office"), "{s}");
        app.popup = Popup::ConfirmCancel { id: "agy:4242".into(), label: "agy-worker".into(), pid: 4242 };
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("Cancel agy-worker (pid 4242)? y/n"));
        app.popup = Popup::View { sel: 1 };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("▶ [x] bubbles") && s.contains("[ ] selected only") && s.contains("bubbles — what each agent is doing"), "{s}");
        app.popup = Popup::Theme { sel: 2, names: vec!["procedural".into(), "modern".into(), "cozy".into()], orig: crate::app::state::MixSnap(app.mix.clone()) };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("▶ 3 cozy") && s.contains("●1 procedural") && s.contains("custom mix…"), "{s}");
        app.popup = Popup::Mixer { layer: 2 };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("▶ furniture") && s.contains("◀ procedural ▶"), "{s}");
    }

    #[test]
    fn inbox_card_header_badge_and_list_render() {
        let (mut app, mut scene) = test_app();
        scene.reports.push(office_core::Report {
            id: 1, at_ms: 60_000, kind: office_core::ReportKind::Result, from: "cc:dice".into(), from_label: "cc·DICE".into(),
            to: None, to_label: "you".into(), title: "Turn done".into(), body: "alpha\n\nbeta\ngamma".into(), brief: String::new(),
        });
        let mut office = OfficeCtx::new(None, None);
        app.observe(&scene, 62_000, &mut FakeControl::default());
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("enter read · esc dismiss · i inbox"), "{s}");
        assert!(s.contains("from cc·DICE") && s.contains("Turn done") && s.contains("alpha"), "{s}");
        assert!(!s.contains("gamma"), "summary only");
        assert!(s.contains("✉ 1"), "{s}");
        assert!(app.layout.card.width > 0);
        app.popup = Popup::Reports { sel: 0, only: None, inbox: true };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("inbox · 1 unread") && s.contains("• "), "{s}");
        assert!(!s.contains("enter read ·"), "no card behind a popup");
    }

    fn reader_scene() -> (App, Scene) {
        let (app, mut scene) = test_app();
        let body = "STATUS: OK\nMODEL: gemini-4-neon-xhigh\nROLE: council-architect\nDURATION_MS: 238000\nRESPONSE:\n# Plan\nFirst **bold** thing with `code` in crates/x/y.rs:123 and https://example.com/a.\n\n## Findings\n- critical: lock is never released\n- minor typo in docs\n";
        scene.reports.push(office_core::Report {
            id: 1, at_ms: 60_000, kind: office_core::ReportKind::Result, from: "cc:dice".into(), from_label: "cc·DICE".into(),
            to: None, to_label: "you".into(), title: "Review: Plan [APPROVE]".into(), body: body.into(), brief: "do it".into(),
        });
        (app, scene)
    }

    #[test]
    fn report_view_is_friendly() {
        let (mut app, scene) = reader_scene();
        let mut office = OfficeCtx::new(None, None);
        app.popup = Popup::ReportView { id: 1, only: None, inbox: true };
        // Summary tab (default): header card chips, status line, key list cards.
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("APPROVE") && s.contains("Review") && s.contains("result"), "{s}");
        assert!(s.contains("cc·DICE → you") && s.contains("took 3 m 58 s") && s.contains("neon-xhigh"), "{s}");
        assert!(s.contains("Summary") && s.contains("Full") && s.contains("Brief") && s.contains("Raw"), "{s}");
        assert!(s.contains("✓ OK · Chief Architect (neon-xhigh) · 3 m 58 s"), "{s}");
        assert!(s.contains("Findings") && s.contains("critical") && s.contains("minor"), "{s}");
        assert!(s.contains("1/1"), "position: {s}");
        assert!(!s.contains("STATUS:") && !s.contains("**") && !s.contains("RESPONSE:"), "{s}");
        // Full tab shows the rendered body.
        app.reader_tab = reader::ReaderTab::Full;
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("• lock is never released") || s.contains("• critical: lock is never released"), "{s}");
        assert!(s.contains("crates/x/y.rs:123"), "{s}");
        // Brief and Raw.
        app.reader_tab = reader::ReaderTab::Brief;
        assert!(render(&mut app, &scene, &mut office, 120, 40).contains("do it"));
        app.reader_tab = reader::ReaderTab::Raw;
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("STATUS: OK") && s.contains("**bold**"), "{s}");
    }

    #[test]
    fn report_view_scrolls_with_a_scrollbar() {
        let (mut app, mut scene) = reader_scene();
        scene.reports[0].body = (0..80).map(|i| format!("line number {i}")).collect::<Vec<_>>().join("\n\n");
        let mut office = OfficeCtx::new(None, None);
        app.popup = Popup::ReportView { id: 1, only: None, inbox: true };
        app.reader_tab = reader::ReaderTab::Full;
        let s = render(&mut app, &scene, &mut office, 120, 30);
        assert!(app.layout.detail_max_scroll > 50, "{}", app.layout.detail_max_scroll);
        assert!(s.contains("█") || s.contains("║"), "scrollbar thumb: {s}");
    }

    #[test]
    fn sidebar_tab_bar_and_stats_gauge_render() {
        let (mut app, scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        let s = render(&mut app, &scene, &mut office, 140, 40);
        for needle in ["1 Agents", "2 Jobs", "3 Activity", "4 Stats", "Agents (4)"] {
            assert!(s.contains(needle), "missing {needle:?}:\n{s}");
        }
        assert!(app.layout.side_tabs.width > 0);
        app.side_tab = SideTab::Stats;
        app.samples.sample(&scene, 1_000);
        let s = render(&mut app, &scene, &mut office, 140, 40);
        for needle in ["4 Stats", "router busy 2/3", "pressure warn", "alerts info 1 · warn 1 · crit 0", "tokens in 14.1k · out 1.0k", "cost 30m", "active agents", "cost per session", "cc·DICE", "$1.00"] {
            assert!(s.contains(needle), "missing {needle:?}:\n{s}");
        }
        assert!(!s.contains("Agents (4)"));
        assert!(app.layout.row_ids.is_empty(), "no agent rows on the stats tab");
        // Narrow sidebar (34 cols) still shows the bar and the gauge.
        let s = render(&mut app, &scene, &mut office, 90, 24);
        assert!(s.contains("4 Stats") && s.contains("router busy 2/3"), "{s}");
    }

    #[test]
    fn jobs_and_activity_tabs_render() {
        let (mut app, mut scene) = test_app();
        for (id, at, title, from, to) in [
            (1u64, 20_000u64, "Memo: Add login", "Chief of Staff", "Chief Architect"),
            (2, 40_000, "Critique: Add login [REVISE]", "Review Seat", "Chief of Staff"),
        ] {
            scene.reports.push(office_core::Report {
                id, at_ms: at, kind: if id == 1 { office_core::ReportKind::Task } else { office_core::ReportKind::Report },
                from: format!("rosso:{from}"), from_label: from.into(), to: Some(format!("rosso:{to}")), to_label: to.into(),
                title: title.into(), body: "x".into(), brief: String::new(),
            });
            scene.report_jobs.insert(id, "job-login".into());
        }
        let mut office = OfficeCtx::new(None, None);
        app.side_tab = SideTab::Jobs;
        let s = render(&mut app, &scene, &mut office, 140, 40);
        for needle in ["Jobs (1)", "job-login", "Add login", "2 docs", "REVISE", "3/7 critique"] {
            assert!(s.contains(needle), "missing {needle:?}:\n{s}");
        }
        app.side_tab = SideTab::Activity;
        let s = render(&mut app, &scene, &mut office, 140, 40);
        assert!(s.contains("Activity (2)") && s.contains("✉") && s.contains("Review Seat → Chief of Staff"), "{s}");
        app.side_tab = SideTab::Jobs;
        scene.reports.clear();
        scene.report_jobs.clear();
        assert!(render(&mut app, &scene, &mut office, 140, 40).contains("No ROSSO jobs yet"));
    }

    #[test]
    fn header_status_strip_shows_when_wide() {
        let (mut app, scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        let s = render(&mut app, &scene, &mut office, 180, 30);
        assert!(s.contains("● 3 active · ✉ 0 · $1.23 · router 2/3"), "{s}");
        let s = render(&mut app, &scene, &mut office, 120, 30);
        assert!(!s.contains("router 2/3"), "too narrow for the strip:\n{s}");
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

    #[test]
    fn jobs_tab_renders_wave_progress_row() {
        let (mut app, mut scene) = test_app();
        scene.reports.push(office_core::Report {
            id: 1,
            at_ms: 40_000,
            kind: office_core::ReportKind::Task,
            from: "rosso:Chief of Staff".into(),
            from_label: "Chief of Staff".into(),
            to: Some("rosso:Chief Architect".into()),
            to_label: "Chief Architect".into(),
            title: "Memo: Add login".into(),
            body: "x".into(),
            brief: String::new(),
        });
        scene.report_jobs.insert(1, "job-login".into());
        scene.waves.insert(
            "job-login".into(),
            office_core::WaveState {
                job: "job-login".into(),
                plan: "council-r2".into(),
                updated_ms: 55_000,
                done: 2,
                relay_inflight: 1,
                pending: 1,
                calls: vec![
                    office_core::WaveCall { title: "Security Seat".into(), status: "OK".into(), verdict: Some("APPROVE".into()), relay: false },
                    office_core::WaveCall { title: "Review Seat".into(), status: "OK".into(), verdict: Some("REVISE".into()), relay: false },
                ],
            },
        );
        scene.waves.insert(
            "job-wave-only".into(),
            office_core::WaveState {
                job: "job-wave-only".into(),
                plan: "inspect".into(),
                updated_ms: 50_000,
                done: 1,
                relay_inflight: 0,
                pending: 1,
                calls: vec![office_core::WaveCall { title: "Tester".into(), status: "OK".into(), verdict: Some("PASS".into()), relay: false }],
            },
        );

        let mut office = OfficeCtx::new(None, None);
        app.side_tab = SideTab::Jobs;
        let s = render(&mut app, &scene, &mut office, 140, 40);
        assert!(s.contains("Jobs (2)"), "{s}");
        assert!(s.contains("wave council-r2: 2✓ 1⟳ 1·"), "{s}");
        assert!(s.contains("APPROVE"), "{s}");
        assert!(s.contains("wave inspect: 1✓ 1· PASS"), "{s}");
    }

    #[test]
    fn inspector_and_detail_show_dispatch_mode_and_queue_ticket() {
        let (mut app, mut scene) = test_app();
        let mut office = OfficeCtx::new(None, None);

        // Dispatch modes on a top-level Claude session
        scene.dispatch_modes.insert("cc:dice".into(), office_core::DispatchMode::Direct);
        select(&mut app, "cc:dice");
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("DIRECT"), "{s}");
        app.popup = Popup::Detail("cc:dice".into());
        let d = render(&mut app, &scene, &mut office, 120, 40);
        assert!(d.contains("dispatch") && d.contains("DIRECT"), "{d}");

        app.popup = Popup::None;
        scene.dispatch_modes.insert("cc:dice".into(), office_core::DispatchMode::Relay);
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("RELAY"), "{s}");

        scene.dispatch_modes.insert("cc:dice".into(), office_core::DispatchMode::Hybrid);
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("DIR+RLY"), "{s}");

        // Queue ticket on queued agy worker with repo_busy
        scene.queue_tickets.insert(
            "agy:4242".into(),
            office_core::QueueTicket { pos: 1, since_ms: 10_000, repo_busy: true },
        );
        select(&mut app, "agy:4242");
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("queue #1 (repo busy)"), "{s}");
        app.popup = Popup::Detail("agy:4242".into());
        let d = render(&mut app, &scene, &mut office, 120, 40);
        assert!(d.contains("queue") && d.contains("queue #1 (repo busy)"), "{d}");

        // Non-busy queue ticket omits "(repo busy)"
        app.popup = Popup::None;
        scene.queue_tickets.insert(
            "agy:4242".into(),
            office_core::QueueTicket { pos: 2, since_ms: 10_000, repo_busy: false },
        );
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("queue #2") && !s.contains("repo busy"), "{s}");
    }

    #[test]
    fn router_lines_show_rsv_and_orphans_only_when_present() {
        let (_, mut scene) = test_app();
        let line_text = |lines: Vec<Line<'static>>| -> String {
            lines.into_iter().map(|l| l.spans.into_iter().map(|s| s.content.into_owned()).collect::<String>()).collect::<Vec<_>>().join("\n")
        };

        let base = line_text(router_lines(&scene, 60));
        assert!(base.contains("free RAM 1800 MB · per agy 280 MB"), "{base}");
        assert!(base.contains("pressure warn · queued 1"), "{base}");
        assert!(base.contains("LIMIT capacity 3 reached"), "{base}");
        assert!(!base.contains("rsv") && !base.contains("orphan"), "{base}");

        scene.router_extra.reserved_mb = Some(0);
        let zero_rsv = line_text(router_lines(&scene, 60));
        assert!(!zero_rsv.contains("rsv"), "{zero_rsv}");

        scene.router_extra.reserved_mb = Some(512);
        scene.router_extra.orphans = 2;
        let extra = line_text(router_lines(&scene, 60));
        assert!(extra.contains("free RAM 1800 MB · per agy 280 MB · rsv 512M"), "{extra}");
        assert!(extra.contains("pressure warn · queued 1 · 2 orphan"), "{extra}");
    }

    #[test]
    fn free_ram_btn_renders_and_records_hit_rect() {
        let (mut app, mut scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        // Idle candidate with PID & RSS makes the strip highlight reclaimable MB.
        if let Some(a) = scene.agents.get_mut("agy:4242") {
            a.state = office_core::AgentState::Idle;
            a.pid = Some(4242);
            a.rss_mb = Some(350);
        }
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("[K] Free RAM (~350 MB · 1 idle)"), "{s}");
        assert!(app.layout.free_ram_btn.width > 0 && app.layout.free_ram_btn.height == 1);

        // Switching to Jobs clears free_ram_btn; hiding sidebar clears it too.
        app.side_tab = SideTab::Jobs;
        let _ = render(&mut app, &scene, &mut office, 120, 40);
        assert_eq!(app.layout.free_ram_btn, Rect::default());

        app.side_tab = SideTab::Agents;
        app.sidebar = false;
        let _ = render(&mut app, &scene, &mut office, 120, 40);
        assert_eq!(app.layout.free_ram_btn, Rect::default());
    }

    #[test]
    fn command_center_popups_render() {
        let (mut app, mut scene) = test_app();
        let mut office = OfficeCtx::new(None, None);

        // FreeRam popup
        let targets = vec![crate::app::state::IdleTarget {
            id: "agy:4242".into(),
            label: "agy-worker".into(),
            pid: 4242,
            rss_mb: 320,
            idle_ms: 90_000,
            is_orphan: false,
        }];
        scene.router_extra.orphans = 2;
        app.popup = Popup::FreeRam { sel: 0, targets, checked: vec![true] };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("Free RAM — kill idle agents"), "{s}");
        assert!(s.contains("Reclaimable: ~320 MB (1 selected) · or press r to reap 2 orphans"), "{s}");
        assert!(s.contains("[x] agy-worker"), "{s}");
        assert!(s.contains("~320 MB"), "{s}");
        assert!(s.contains("Space toggle · a all · r reap orphans · Enter kill · Esc cancel"), "{s}");

        // Dispatch popup (new task & follow-up)
        app.popup = Popup::Dispatch {
            field: 0,
            target_idx: 0,
            cwd: "/tmp/repo".into(),
            prompt: "audit tests".into(),
            followup: None,
        };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("Dispatch Task") && s.contains("Target:  ◀ agy worker (edit) ▶"), "{s}");
        assert!(s.contains("CWD:     /tmp/repo") && s.contains("Prompt:  audit tests"), "{s}");

        app.popup = Popup::Dispatch {
            field: 2,
            target_idx: 0,
            cwd: "/tmp/repo".into(),
            prompt: "continue".into(),
            followup: Some(("cc:dice".into(), "cc·DICE".into())),
        };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("Reply to cc·DICE") && s.contains("Reply:   cc·DICE"), "{s}");
        assert!(s.contains("Prompt:  continue▌"), "{s}");

        // Actions popup
        app.popup = Popup::Actions { sel: 0 };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("Command Center") && s.contains("[p]  Dispatch new task"), "{s}");
        assert!(s.contains("[K]  Free RAM — kill idle agents"), "{s}");

        // ConfirmApproveAll popup
        app.popup = Popup::ConfirmApproveAll {
            items: vec![
                ("cc:office".into(), "cc·office".into(), "req-1".into()),
                ("cc:dice".into(), "cc·DICE".into(), "req-2".into()),
            ],
        };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("confirm approve all"), "{s}");
        assert!(s.contains("Approve all 2 pending permissions? y/n"), "{s}");
        assert!(s.contains("cc·office, cc·DICE"), "{s}");

        // ConfirmCancel includes ~rss MB when present
        scene.agents.get_mut("agy:4242").unwrap().rss_mb = Some(280);
        app.popup = Popup::ConfirmCancel { id: "agy:4242".into(), label: "agy-worker".into(), pid: 4242 };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("Cancel agy-worker (pid 4242, ~280 MB)? y/n"), "{s}");

        // ConfirmAbortAuto popup
        app.popup = Popup::ConfirmAbortAuto { job: "j-auto".into(), phase: "execute".into() };
        assert!(popup_rect(Rect::new(0, 0, 120, 40), &app, &scene).is_some());
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("confirm abort auto"), "{s}");
        assert!(s.contains("Abort auto job j-auto (phase execute)?"), "{s}");
        assert!(s.contains("Y / Enter abort · N / Esc cancel"), "{s}");
    }

    #[test]
    fn jobs_panel_renders_auto_job_status_ruling_and_error() {
        let (mut app, mut scene) = test_app();
        let mut office = OfficeCtx::new(None, None);
        app.side_tab = SideTab::Jobs;

        scene.auto_jobs.push(office_core::AutoJob {
            job: "j-ruling".into(),
            task: "Ship Stage 2C".into(),
            flow: "council".into(),
            phase: "execute".into(),
            status: "running".into(),
            calls_used: 4,
            max_calls: 12,
            started_at: 60_000,
            ruling: Some("APPROVE".into()),
            ..Default::default()
        });
        scene.auto_jobs.push(office_core::AutoJob {
            job: "j-err".into(),
            task: "Fix retry".into(),
            flow: "simple".into(),
            phase: "inspect".into(),
            status: "failed".into(),
            calls_used: 2,
            max_calls: 8,
            started_at: 50_000,
            last_error: Some("timeout".into()),
            ..Default::default()
        });

        let s = render(&mut app, &scene, &mut office, 140, 40);
        for needle in [
            "Jobs (2)",
            "j-ruling",
            "AUTO council execute (6/8) 4/12",
            "APPROVE · [R]esume / [X]abort",
            "j-err",
            "AUTO simple inspect (4/5) 2/8",
            "err: timeout · [R]esume / [X]abort",
        ] {
            assert!(s.contains(needle), "missing {needle:?}:\n{s}");
        }
    }

    #[test]
    fn ui_palette_syncs_with_theme_chrome_and_theme_picker_shows_swatches() {
        let (mut app, scene) = test_app();
        app.mix = crate::app::state::uniform_mix("tokyo-night");
        let themed = ui_palette(&app);
        assert_eq!(themed.accent, Color::Rgb(0x7A, 0xA2, 0xF7));
        app.show.theme_chrome = false;
        let neutral = ui_palette(&app);
        assert_eq!(neutral.accent, Color::Rgb(0xE0, 0x7A, 0x3F));

        let mut office = OfficeCtx::new(None, None);
        app.popup = Popup::Theme {
            sel: 1,
            names: vec!["procedural".into(), "tokyo-night".into(), "nord".into(), "custom-user".into()],
            orig: crate::app::state::MixSnap(app.mix.clone()),
        };
        let s = render(&mut app, &scene, &mut office, 120, 40);
        assert!(s.contains("tokyo-night") && s.contains("indigo dark studio") && s.contains("████"), "{s}");
        assert!(s.contains("nord") && s.contains("arctic slate frost"), "{s}");
        assert!(s.contains("custom-user") && s.contains("custom theme"), "{s}");
    }
}
