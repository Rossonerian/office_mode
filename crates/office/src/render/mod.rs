//! Workstream B. Theme loading + scene compositing into an RGBA framebuffer.
//!
//! The office is composed at 1× in logical pixels (16 px tiles), then nearest-neighbour scaled
//! by an integer factor (min 1; fractional only when even 1× does not fit) into the target size.
//! Static layers (floor, walls, decor, rugs, chairs) are cached per theme mix + room geometry.
#![allow(dead_code)]

pub mod canvas;
pub mod cat;
pub mod ceo;
pub mod character;
pub mod demo;
pub mod extras;
pub mod font;
pub mod layout;
pub mod procedural;
pub mod theme;

use canvas::{add, blit, fill, outline, put, rgb, shade, Col};
use character::{Arms, Legs, Pose};
use image::RgbaImage;
use layout::{Layout, CELL_H, CELL_W, DECOR_H, MARGIN, POD_H, POD_W, WALL_H};
use office_core::{config::{CatConfig, CeoConfig, Config, ShowConfig, ThemeMix}, model::AgentKind, Agent, AgentId, AgentState, Scene};
use std::collections::HashMap;
use std::path::PathBuf;
use theme::Theme;

pub const KIND_CLAUDE: Col = rgb(0xe8773a);
pub const KIND_SUB: Col = rgb(0xf2b134);
pub const KIND_AGY: Col = rgb(0x2ec4b6);
pub const KIND_OTHER: Col = rgb(0x9aa0a6);
const SELECT: Col = rgb(0xffe14d);
const INK: Col = rgb(0x1b1d22);
const PAPER: Col = rgb(0xfbfaf5);
/// Pseudo agent ids that `hit_test` returns for the ambient characters.
pub const CAT_ID: &str = "office:cat";
pub const CEO_ID: &str = "office:ceo";

pub fn kind_color(k: AgentKind) -> Col {
    match k {
        AgentKind::Claude => KIND_CLAUDE,
        AgentKind::ClaudeSub => KIND_SUB,
        AgentKind::Agy => KIND_AGY,
        AgentKind::Other => KIND_OTHER,
    }
}

/// How long the walk in/out takes per pixel of distance, and the minimum.
const WALK_PX_PER_S: f32 = 70.0;
const WALK_MIN_MS: f32 = 700.0;
const DONE_CHEER_MS: u64 = 1500;

#[derive(Debug, Clone, Copy)]
struct View {
    ox: f32,
    oy: f32,
    s: f32,
}

/// Composed 1× logical pixel-art frame plus target placement parameters.
pub struct LogicalFrame<'a> {
    pub img: &'a RgbaImage,
    pub scale: f32,
    pub ox: u32,
    pub oy: u32,
    pub px_w: u32,
    pub px_h: u32,
    pub backdrop: Col,
    pub is_walking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// At the desk (seated or standing behind it).
    Desk { standing: bool },
    /// Free-standing at feet position (fx, fy) with optional 4-phase walk frame.
    Free { fx: i32, fy: i32, walk: Option<u8> },
    Hidden,
}

pub struct Renderer {
    mix: ThemeMix,
    dirs: Vec<PathBuf>,
    themes: HashMap<String, Theme>,
    errors: Vec<String>,
    static_key: (u64, u64),
    static_img: RgbaImage,
    cached_layout: Option<(u64, u32, u32, Layout)>,
    frame_buf: RgbaImage,
    out_buf: RgbaImage,
    xmap: Vec<usize>,
    hits: Vec<([i32; 4], AgentId)>,
    view: View,
    logical: (u32, u32),
    cat: CatConfig,
    ceo: CeoConfig,
    show: ShowConfig,
    ceo_look: ceo::CeoLook,
    hover: Option<AgentId>,
    /// Interior mutability: the cat continues its walk across frames while themes are borrowed.
    cat_mem: std::cell::RefCell<cat::Memory>,
    /// Sessions visiting the boss for a plan approval: id -> (walk start, approval end).
    visit_mem: std::cell::RefCell<HashMap<AgentId, (u64, Option<u64>)>>,
}

/// How long a courier or a boss visit may stay on screen at most.
const TRIP_MAX_MS: u64 = 20_000;
/// How long a session stands at the boss's desk presenting a finished turn.
const PRESENT_MS: u64 = 2_500;
/// How long a failed call's red bubble and hint stay on its agent.
const FAILURE_SHOW_MS: u64 = 60_000;

/// A session's trip to the boss's office.
#[derive(Debug, Clone, Copy)]
struct BossTrip {
    place: Place,
    /// Standing at the boss's desk (not walking).
    at_boss: bool,
    /// Waiting for a plan approval (vs presenting a finished turn).
    approval: bool,
}

/// Total walking time of a route (matches walk_along).
fn route_ms(pts: &[(i32, i32)]) -> u64 {
    let total: f32 = pts.windows(2).map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as f32).sum();
    (total / WALK_PX_PER_S * 1000.0).max(WALK_MIN_MS) as u64
}

/// Where a visitor stands in the boss's office: in front of the desk, left of centre.
fn boss_spot(lay: &Layout) -> Option<(i32, i32)> {
    lay.ceo.map(|[cx, cy, cw, ch]| (cx + cw / 2 - 14, cy + ch - 2))
}

/// The floor point in front of an agent's desk.
fn desk_aisle(lay: &Layout, id: &str) -> Option<(i32, i32)> {
    lay.seats.get(id).map(|s| lay.seat_aisle(s))
}

impl Renderer {
    pub fn new(cfg: &Config) -> Self {
        Self::with_dirs(cfg, theme::search_dirs())
    }

    /// Like `new`, with an explicit theme search path.
    pub fn with_dirs(cfg: &Config, dirs: Vec<PathBuf>) -> Self {
        let mut r = Renderer {
            mix: cfg.theme.clone(),
            dirs,
            themes: HashMap::new(),
            errors: Vec::new(),
            static_key: (0, 0),
            static_img: RgbaImage::new(1, 1),
            cached_layout: None,
            frame_buf: RgbaImage::new(1, 1),
            out_buf: RgbaImage::new(1, 1),
            xmap: Vec::new(),
            hits: Vec::new(),
            view: View { ox: 0.0, oy: 0.0, s: 1.0 },
            logical: (0, 0),
            cat: cfg.cat.clone(),
            ceo: cfg.ceo.clone(),
            show: cfg.show.clone(),
            ceo_look: ceo::look(&cfg.ceo),
            hover: None,
            cat_mem: Default::default(),
            visit_mem: Default::default(),
        };
        r.ensure_mix();
        r
    }

    /// All available theme names (always includes "procedural").
    pub fn themes(&self) -> Vec<String> {
        theme::list(&self.dirs)
    }

    pub fn set_mix(&mut self, mix: &ThemeMix) {
        self.mix = mix.clone();
        self.static_key = (0, 0);
        self.ensure_mix();
    }

    /// Apply the display toggles (names, bubbles, zones, ...); repaints the static layer when needed.
    pub fn set_show(&mut self, show: &ShowConfig) {
        if self.show != *show {
            self.show = show.clone();
            self.static_key = (0, 0);
            self.cached_layout = None;
        }
    }

    fn ceo_on(&self) -> bool {
        self.ceo.enabled && self.show.ceo
    }

    fn cat_on(&self) -> bool {
        self.cat.enabled && self.show.cat
    }

    /// Names/bubbles may be limited to the selected or hovered agent and agents needing attention.
    fn label_gate(&self, focus: bool) -> bool {
        !self.show.selected_only_labels || focus
    }

    /// What the mouse is over (shows the cat's name, highlights the CEO).
    pub fn set_hover(&mut self, id: Option<AgentId>) {
        self.hover = id;
    }

    pub fn mix(&self) -> &ThemeMix {
        &self.mix
    }

    /// Theme load errors so far (a failing theme falls back to procedural).
    pub fn theme_errors(&self) -> &[String] {
        &self.errors
    }

    /// Logical (1×) size of the last composed office.
    pub fn logical_size(&self) -> (u32, u32) {
        self.logical
    }

    /// Scale factor used in the last frame (integer unless the office had to shrink below 1×).
    pub fn scale(&self) -> f32 {
        self.view.s
    }

    /// Bounding box (x, y, w, h) in output pixels of an agent in the last frame.
    pub fn agent_rect(&self, id: &AgentId) -> Option<(u32, u32, u32, u32)> {
        let v = self.view;
        self.hits.iter().find(|(_, h)| h == id).map(|([x, y, w, h], _)| {
            (
                (*x as f32 * v.s + v.ox).max(0.0) as u32,
                (*y as f32 * v.s + v.oy).max(0.0) as u32,
                (*w as f32 * v.s) as u32,
                (*h as f32 * v.s) as u32,
            )
        })
    }

    fn ensure_mix(&mut self) {
        let names = [&self.mix.floor, &self.mix.walls, &self.mix.furniture, &self.mix.decor, &self.mix.characters]
            .map(|s| s.clone());
        for n in names {
            if !self.themes.contains_key(&n) {
                let t = match theme::load(&n, &self.dirs) {
                    Ok(t) => t,
                    Err(e) => {
                        self.errors.push(format!("{e:#}"));
                        let mut p = theme::procedural();
                        p.name = n.clone();
                        p
                    }
                };
                self.themes.insert(n, t);
            }
        }
    }

    fn mix_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for s in [&self.mix.floor, &self.mix.walls, &self.mix.furniture, &self.mix.decor, &self.mix.characters] {
            for b in s.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn topology_hash(&self, scene: &Scene) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut feed_bytes = |bs: &[u8]| {
            for &b in bs {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x100000001b3);
        };
        feed_bytes(&[self.ceo_on() as u8, self.show.council_cabin as u8]);
        for a in scene.agents.values() {
            feed_bytes(a.id.as_bytes());
            feed_bytes(&[a.kind as u8]);
            feed_bytes(a.parent.as_deref().unwrap_or("").as_bytes());
            feed_bytes(a.cwd.as_deref().unwrap_or("").as_bytes());
            feed_bytes(&a.started_ms.to_le_bytes());
        }
        h
    }

    /// Compose the office at 1× logical resolution into an internal reusable buffer.
    pub fn compose_logical(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> LogicalFrame<'_> {
        self.ensure_mix();
        let (px_w, px_h) = (px_w.max(1), px_h.max(1));
        let topo = self.topology_hash(scene);
        let lay = match &self.cached_layout {
            Some((k, w, h, cached)) if *k == topo && *w == px_w && *h == px_h => cached.clone(),
            _ => {
                let built = layout::build_with(scene, px_w, px_h, layout::Extras { ceo: self.ceo_on(), council: self.show.council_cabin });
                self.cached_layout = Some((topo, px_w, px_h, built.clone()));
                built
            }
        };
        let night = self.is_night(t_ms);
        let key = (self.mix_hash() ^ ((self.show.zones as u64) << 40) ^ ((night as u64) << 41), lay.static_hash());
        if key != self.static_key {
            self.static_img = self.draw_static(&lay, night);
            self.static_key = key;
        }
        let (lw_u, lh_u) = (self.static_img.width(), self.static_img.height());
        if self.frame_buf.dimensions() != (lw_u, lh_u) {
            self.frame_buf = RgbaImage::new(lw_u, lh_u);
        }
        self.frame_buf.as_mut().copy_from_slice(self.static_img.as_raw());
        let mut frame = std::mem::replace(&mut self.frame_buf, RgbaImage::new(1, 1));
        self.hits.clear();
        let is_walking = self.draw_dynamic(&mut frame, scene, &lay, selected, t_ms);
        self.frame_buf = frame;

        let (lw, lh) = (lay.w as f32, lay.h as f32);
        let fit = (px_w as f32 / lw).min(px_h as f32 / lh);
        let s = if fit >= 1.0 { fit.floor() } else { fit };
        let (ow, oh) = ((lw * s).floor(), (lh * s).floor());
        let ox = ((px_w as f32 - ow) / 2.0).floor().max(0.0);
        let oy = ((px_h as f32 - oh) / 2.0).floor().max(0.0);
        let backdrop = self.th(Layer::Walls).fx.apply(self.th(Layer::Walls).colors.backdrop);
        self.view = View { ox, oy, s };
        self.logical = (lay.w as u32, lay.h as u32);
        LogicalFrame { img: &self.frame_buf, scale: s, ox: ox as u32, oy: oy as u32, px_w, px_h, backdrop, is_walking }
    }

    /// Compose the office for `scene` at time `t_ms` into a px_w × px_h image.
    /// `selected` gets a highlight ring.
    pub fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage {
        let lf = self.compose_logical(scene, selected, px_w, px_h, t_ms);
        let (s, ox, oy, bg) = (lf.scale, lf.ox, lf.oy, lf.backdrop);
        if self.out_buf.dimensions() != (px_w.max(1), px_h.max(1)) {
            self.out_buf = RgbaImage::new(px_w.max(1), px_h.max(1));
        }
        for px in self.out_buf.as_mut().chunks_exact_mut(4) {
            px.copy_from_slice(&bg);
        }
        canvas::scale_into_with_xmap(&mut self.out_buf, &self.frame_buf, ox, oy, s, &mut self.xmap);
        self.out_buf.clone()
    }

    /// Which agent (if any) is drawn at pixel (x, y) in the last composed frame (for mouse clicks).
    pub fn hit_test(&self, x: u32, y: u32) -> Option<AgentId> {
        let v = self.view;
        let lx = ((x as f32 - v.ox) / v.s).floor() as i32;
        let ly = ((y as f32 - v.oy) / v.s).floor() as i32;
        self.hits
            .iter()
            .rev()
            .find(|([hx, hy, hw, hh], _)| lx >= *hx && ly >= *hy && lx < hx + hw && ly < hy + hh)
            .map(|(_, id)| id.clone())
    }

    fn mix_names(&self) -> [&str; 5] {
        [&self.mix.floor, &self.mix.walls, &self.mix.furniture, &self.mix.decor, &self.mix.characters].map(|s| s.as_str())
    }

    fn th(&self, layer: Layer) -> &Theme {
        let n = match layer {
            Layer::Floor => &self.mix.floor,
            Layer::Walls => &self.mix.walls,
            Layer::Furniture => &self.mix.furniture,
            Layer::Decor => &self.mix.decor,
            Layer::Characters => &self.mix.characters,
        };
        &self.themes[n]
    }

    fn is_night(&self, t_ms: u64) -> bool {
        if self.mix.walls == "midnight" || self.mix.walls == "cyberpunk" {
            return true;
        }
        // Keep test/preview synthetic timestamps deterministic daytime; live epoch timestamps check local hour.
        if t_ms < 1_000_000_000_000 {
            return false;
        }
        #[cfg(unix)]
        {
            let secs = (t_ms / 1000) as libc::time_t;
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&secs, &mut tm) };
            tm.tm_hour >= 20 || tm.tm_hour < 6
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    // ---------------------------------------------------------------- static layer

    fn draw_static(&self, lay: &Layout, night: bool) -> RgbaImage {
        let (w, h) = (lay.w, lay.h);
        let mut img = RgbaImage::new(w as u32, h as u32);
        let floor_t = self.th(Layer::Floor);
        let walls_t = self.th(Layer::Walls);
        let furn_t = self.th(Layer::Furniture);
        let decor_t = self.th(Layer::Decor);
        let night_theme = self.mix.walls == "midnight" || self.mix.walls == "cyberpunk";
        let _ = night;

        // floor
        if let Some(bg) = floor_t.floor_bg {
            fill(&mut img, 0, WALL_H, w, h - WALL_H, bg);
        }
        let ft = floor_t.sprite("floor").frame(0).clone();
        tile(&mut img, &ft, 0, WALL_H, w, h - WALL_H);

        // back wall
        if let Some(bg) = walls_t.wall_bg {
            fill(&mut img, 0, 0, w, WALL_H, bg);
        }
        let wt = walls_t.sprite("wall").frame(0).clone();
        tile(&mut img, &wt, 0, 0, w, WALL_H);
        let trim = walls_t.fx.apply(walls_t.colors.wall_trim);
        fill(&mut img, 0, WALL_H - 3, w, 3, trim);
        fill(&mut img, 0, WALL_H, w, 1, shade(trim, 0.6));

        // whiteboard (decor) centred on the wall; windows (walls) either side
        let wb = decor_t.sprite("whiteboard");
        let wb_x = w / 2 - wb.w() / 2;
        let win = walls_t.sprite("window");
        let step = win.w() + 18;
        let sky = walls_t.fx.apply(walls_t.colors.sky);
        let mut x = 14;
        while x + win.w() <= w - 12 {
            let clear_of_board = x + win.w() + 6 < wb_x || x > wb_x + wb.w() + 6;
            if clear_of_board {
                let wy = (WALL_H - 3 - win.h()) / 2;
                blit(&mut img, win.frame(0), x, wy);
                if let (Some(view), Some(glass)) = (walls_t.sprite_opt("window_view"), win.screen) {
                    draw_window_view(&mut img, walls_t, view, [x + glass[0], wy + glass[1], glass[2], glass[3]], night, x);
                }
                if night_theme && walls_t.fx.palette.is_empty() && win.w() >= 20 {
                    put(&mut img, x + 5, wy + 4, with_alpha(PAPER, 190));
                    put(&mut img, x + win.w() - 7, wy + 7, with_alpha(PAPER, 160));
                }
                if walls_t.fx.palette.is_empty() {
                    // soft diagonal light shaft onto the upper floor strip
                    let shaft_col = with_alpha(sky, if night_theme { 12 } else { 20 });
                    for dy in 1..=16 {
                        fill(&mut img, x + dy / 2, WALL_H + dy, win.w() - 2, 1, shaft_col);
                    }
                }
            }
            x += step;
        }
        contact_shadow(&mut img, decor_t, wb_x + 1, (WALL_H - wb.h()).max(2) / 2 + 2 + wb.h(), wb.w() - 2, 2);
        blit(&mut img, wb.frame(0), wb_x, (WALL_H - wb.h()).max(2) / 2 + 2);

        // company plaque: dark plate, light border, red jersey stripe; centred above the whiteboard
        {
            let fx = &walls_t.fx;
            let name = "ROSSO";
            let (tw, ph) = (font::text_width(name), font::GLYPH_H + 5);
            let pw = tw + 8;
            let (px, py) = (w / 2 - pw / 2, 2);
            fill(&mut img, px, py, pw, ph, fx.apply(rgb(0xd8d4c8)));
            fill(&mut img, px + 1, py + 1, pw - 2, ph - 2, fx.apply(rgb(0x14161a)));
            fill(&mut img, px + 1, py + 1, pw - 2, 1, fx.apply(rgb(0xc8102e)));
            font::draw_text(&mut img, px + 4, py + 3, name, fx.apply(rgb(0xf4f1e8)));
        }

        // bottom wall strip with the door at the bottom-left
        let bh = 10;
        if let Some(bg) = walls_t.wall_bg {
            fill(&mut img, 0, h - bh, w, bh, bg);
        }
        tile(&mut img, &wt, 0, h - bh, w, bh);
        fill(&mut img, 0, h - bh, w, 2, trim);
        let door = walls_t.sprite("door");
        let door_x = lay.door.0 - door.w() / 2;
        let door_y = if door.h() <= 16 { h - bh - door.h() + 6 } else { h - door.h() };
        blit(&mut img, door.frame(0), door_x, door_y);

        // waiting bench along the bottom lobby corridor
        let mut bench = procedural::bench(&furn_t.colors);
        if !furn_t.fx.is_identity() {
            canvas::map_pixels(&mut bench, &|c| furn_t.fx.apply(c));
        }
        let (bw, bhh) = (bench.width() as i32, bench.height() as i32);
        let bx = lay.lobby_bench.0 - 6;
        let by = lay.lobby_bench.1 - bhh + 2;
        if bx + bw < w - 30 {
            contact_shadow(&mut img, furn_t, bx + 1, by + bhh - 1, bw - 2, 2);
            blit(&mut img, &bench, bx, by);
        }

        // decor strip hugging the back wall (`WALL_H + 26`), keeping >=12px clear above row-0 speech bubbles
        let strip_bottom = WALL_H + 26;
        let plant = decor_t.sprite("plant");
        contact_shadow(&mut img, decor_t, 5, strip_bottom - 1, plant.w() - 2, 2);
        blit(&mut img, plant.frame(0), 4, strip_bottom - plant.h());
        contact_shadow(&mut img, decor_t, w - 3 - plant.w(), strip_bottom - 1, plant.w() - 2, 2);
        blit(&mut img, plant.frame(0), w - 4 - plant.w(), strip_bottom - plant.h());
        let cooler = furn_t.sprite("cooler");
        let cooler_x = 4 + plant.w() + 8;
        contact_shadow(&mut img, furn_t, cooler_x + 1, strip_bottom - 1, cooler.w() - 2, 2);
        blit(&mut img, cooler.frame(0), cooler_x, strip_bottom - cooler.h());

        let sofa = furn_t.sprite("sofa");
        let sofa_x = w - layout::SOFA_CX_FROM_RIGHT - sofa.w() / 2;
        let sofa_y = WALL_H + DECOR_H - 2 - sofa.h();
        // coffee / espresso bar between cooler and sofa
        let mut coffee = procedural::coffee_bar(&furn_t.colors);
        if !furn_t.fx.is_identity() {
            canvas::map_pixels(&mut coffee, &|c| furn_t.fx.apply(c));
        }
        let (cw, ch_bar) = (coffee.width() as i32, coffee.height() as i32);
        let cb_x = lay.coffee_bar.0 - cw / 2;
        if cb_x > cooler_x + cooler.w() + 6 && cb_x + cw + 10 < sofa_x {
            contact_shadow(&mut img, furn_t, cb_x + 1, strip_bottom - 1, cw - 2, 2);
            blit(&mut img, &coffee, cb_x, strip_bottom - ch_bar);
        }
        contact_shadow(&mut img, furn_t, sofa_x + 2, sofa_y + sofa.h() - 1, sofa.w() - 4, 2);
        blit(&mut img, sofa.frame(0), sofa_x, sofa_y);
        // bottom-right plant by the door corridor
        contact_shadow(&mut img, decor_t, w - 3 - plant.w(), h - bh + 1, plant.w() - 2, 2);
        blit(&mut img, plant.frame(0), w - 4 - plant.w(), h - bh - plant.h() + 2);

        // server rack in the reserved space below the CEO corner office (pod-free column)
        if let Some([_, cy, _, ch]) = lay.ceo {
            let mut rack = procedural::server_rack(&furn_t.colors);
            if !furn_t.fx.is_identity() {
                canvas::map_pixels(&mut rack, &|c| furn_t.fx.apply(c));
            }
            let (rw, rh) = (rack.width() as i32, rack.height() as i32);
            let rx = w - 20;
            let ry = cy + ch + 8;
            if ry + rh + 4 < h - bh - plant.h() {
                contact_shadow(&mut img, furn_t, rx + 1, ry + rh - 1, rw - 2, 2);
                blit(&mut img, &rack, rx, ry);
            }
        }

        // per-session floor zones: a tinted patch with a border and a nameplate band on top
        for z in lay.zones.iter().filter(|_| self.show.zones) {
            let c = furn_t.fx.apply(zone_color(z));
            let [zx, zy, zw, zh] = z.rect;
            fill(&mut img, zx, zy, zw, zh, with_alpha(c, 52));
            outline(&mut img, zx, zy, zw, zh, with_alpha(c, 200));
            fill(&mut img, zx, zy, zw, layout::ZONE_HEAD - 1, with_alpha(shade(c, 0.55), 230));
            fill(&mut img, zx, zy + layout::ZONE_HEAD - 2, zw, 1, with_alpha(c, 230));
        }

        // pods: light carpet tiles, task chairs, frosted privacy partitions between the seats
        let mut cc = furn_t.colors.clone();
        cc.floor_a = floor_t.colors.floor_a;
        cc.rug = decor_t.colors.rug;
        let themed_fx = |mut i: RgbaImage| {
            if !furn_t.fx.is_identity() {
                canvas::map_pixels(&mut i, &|c| furn_t.fx.apply(c));
            }
            i
        };
        let carpet = themed_fx(procedural::carpet(&cc, POD_W - 4, POD_H - 12));
        let panel = themed_fx(procedural::partition(&furn_t.colors, 14));
        let bridge = themed_fx(procedural::desk_bridge(&furn_t.colors));
        let chair = furn_t.sprite("chair");
        for pod in &lay.pods {
            if pod.kind == layout::PodKind::Council {
                draw_council_cabin(&mut img, [floor_t, walls_t, furn_t, decor_t], pod.x, pod.y);
                for d in 0..2 {
                    let (cx, cy) = pod.cell(d);
                    blit(&mut img, chair.frame(0), cx + 26 - chair.w() / 2, cy + 35 - chair.h());
                }
                continue;
            }
            let (px, py) = (pod.x + 2, pod.y + 8);
            soft_shadow(&mut img, furn_t, px + 2, py + carpet.height() as i32 - 1, carpet.width() as i32 - 1, 2);
            blit(&mut img, &carpet, px, py);
            for d in 0..layout::DESKS_PER_POD {
                let (cx, cy) = pod.cell(d);
                let chx = cx + 26 - chair.w() / 2;
                let chy = cy + 35 - chair.h();
                contact_shadow(&mut img, furn_t, chx + 2, chy + chair.h() - 1, chair.w() - 4, 2);
                blit(&mut img, chair.frame(0), chx, chy);
            }
            for r in 0..2 {
                let (ox, oy) = pod.cell(r * 2);
                // desk bench joint with a frosted privacy panel standing on it
                soft_shadow(&mut img, furn_t, ox + CELL_W - 3, oy + 47, 6, 2);
                blit(&mut img, &bridge, ox + CELL_W - 4, oy + 32);
                soft_shadow(&mut img, furn_t, ox + CELL_W, oy + 34, 3, 1);
                blit(&mut img, &panel, ox + CELL_W - 2, oy + 22);
            }
        }
        if let Some(r) = lay.ceo {
            self.draw_ceo_office(&mut img, r);
        }
        let _ = MARGIN;
        img
    }

    /// Static part of the CEO office: parquet floor with a rug, wood-panelled back wall with a framed
    /// picture, a bookcase with coloured spines and a trophy, glass partition with a side door,
    /// the high-back leather chair, two visitor chairs and a plant.
    fn draw_ceo_office(&self, img: &mut RgbaImage, [cx, cy, cw, ch]: [i32; 4]) {
        let decor = self.th(Layer::Decor);
        let walls = self.th(Layer::Walls);
        let furn = self.th(Layer::Furniture);
        let fd = |c: Col| decor.fx.apply(c);
        let ff = |c: Col| furn.fx.apply(c);
        let fw = |c: Col| walls.fx.apply(c);
        let wood = furn.colors.wood;
        let (rx, ry, rw, rh) = (cx - 2, cy - 1, cw + 4, ch + 1);
        // parquet: staggered planks in a dark, warm wood
        let base = shade(wood, 0.82);
        fill(img, rx, ry, rw, rh, ff(base));
        for r in 0..(rh + 3) / 4 {
            let y = ry + r * 4;
            let tone = if r % 2 == 0 { 1.0 } else { 0.93 };
            fill(img, rx, y, rw, 3, ff(shade(base, tone)));
            fill(img, rx, y + 3, rw, 1, ff(shade(base, 0.72)));
            let off = (r * 13) % 22;
            let mut sx = rx + off;
            while sx < rx + rw {
                fill(img, sx, y, 1, 3, ff(shade(base, 0.74)));
                sx += 22;
            }
        }
        // rug in front of the desk (burgundy with a light border and fringe)
        let rug = shade(decor.colors.rug, 0.85);
        let (ux, uy, uw, uh) = (cx + 6, cy + 48, cw - 12, 15);
        fill(img, ux, uy, uw, uh, fd(shade(rug, 0.6)));
        fill(img, ux + 1, uy + 1, uw - 2, uh - 2, fd(rug));
        outline(img, ux + 2, uy + 2, uw - 4, uh - 4, fd(decor.colors.rug_trim));
        for k in (ux + 3..ux + uw - 3).step_by(4) {
            put(img, k, uy + 4, fd(shade(decor.colors.rug_trim, 0.85)));
            put(img, k + 1, uy + uh - 5, fd(shade(decor.colors.rug_trim, 0.85)));
        }
        // back wall: vertical wood panelling under a moulding, lit from the top-left
        let panel = shade(wood, 0.5);
        fill(img, rx, ry, rw, 11, ff(panel));
        for k in (rx..rx + rw).step_by(5) {
            fill(img, k, ry + 1, 1, 10, ff(shade(panel, 0.78)));
            fill(img, k + 1, ry + 1, 1, 10, ff(shade(panel, 1.08)));
        }
        fill(img, rx, ry, rw, 1, ff(shade(wood, 1.2)));
        fill(img, rx, ry + 10, rw, 1, ff(shade(wood, 0.35)));
        fill(img, rx, ry + 11, rw, 1, with_alpha(INK, 70));
        // framed picture (a sunrise over hills)
        let (px, py) = (cx + 25, cy + 1);
        fill(img, px, py, 16, 9, ff(rgb(0xc8a45a)));
        fill(img, px + 1, py + 1, 14, 7, ff(rgb(0x9fd0ee)));
        fill(img, px + 1, py + 5, 14, 3, ff(rgb(0x4f9a5a)));
        fill(img, px + 3, py + 4, 5, 2, ff(rgb(0x3e8049)));
        fill(img, px + 10, py + 2, 3, 3, ff(rgb(0xffd96a)));
        fill(img, px, py + 8, 16, 1, ff(rgb(0x8a6a2c)));
        // bookcase against the right wall: dark carcass, four shelves of coloured spines, a trophy
        let (bx, by, bw, bh) = (cx + 46, cy + 1, 23, 31);
        let case = shade(wood, 0.62);
        soft_shadow(img, furn, bx + 2, by + bh, bw - 1, 2);
        fill(img, bx, by, bw, bh, ff(shade(case, 0.55)));
        fill(img, bx + 1, by + 1, bw - 2, bh - 1, ff(shade(case, 0.35)));
        let spines = [0xc0392b, 0x2c6fb7, 0xe0a83a, 0x3f8f5a, 0x8f5fc8, 0xd9d4c7, 0xd0583f, 0x2f9fa8, 0x55606e];
        for shelf in 0..4 {
            let sy = by + 2 + shelf * 7;
            let mut x = bx + 2;
            let mut k = (shelf * 5) as usize;
            while x < bx + bw - 3 {
                let bwid = 2 + (k % 3) as i32 / 2 + (k % 2) as i32;
                let bhgt = 4 + ((k * 7) % 3) as i32;
                let c = spines[(k * 4 + shelf as usize) % spines.len()];
                if shelf == 0 && x > bx + bw - 12 {
                    break; // room for the trophy on the top shelf
                }
                if x + bwid > bx + bw - 2 {
                    break;
                }
                fill(img, x, sy + 5 - bhgt, bwid, bhgt, ff(rgb(c)));
                fill(img, x, sy + 5 - bhgt, 1, bhgt, ff(shade(rgb(c), 1.25)));
                put(img, x + bwid - 1, sy + 5 - bhgt, ff(shade(rgb(c), 0.7)));
                x += bwid;
                k += 1;
            }
            fill(img, bx + 1, sy + 5, bw - 2, 1, ff(shade(case, 1.1)));
        }
        let gold = ff(rgb(0xe8c547));
        let (tx, ty) = (bx + bw - 10, by + 2);
        fill(img, tx, ty, 5, 3, gold);
        fill(img, tx + 1, ty + 3, 3, 1, ff(shade(rgb(0xe8c547), 0.8)));
        fill(img, tx + 2, ty + 4, 1, 1, gold);
        fill(img, tx, ty, 1, 3, ff(shade(rgb(0xe8c547), 1.25)));
        // glass partition on the left (aluminium rails) with a door gap at the bottom
        let glass = with_alpha(fw(walls.colors.sky), 110);
        let alu = fw(rgb(0xd9dee4));
        let alu_d = fw(rgb(0x7e8791));
        let door_y = cy + 38;
        fill(img, cx - 7, cy - 2, 3, door_y - cy + 2, glass);
        fill(img, cx - 7, cy - 2, 1, door_y - cy + 2, alu);
        fill(img, cx - 5, cy - 2, 1, door_y - cy + 2, alu_d);
        for k in (6..door_y - cy).step_by(14) {
            fill(img, cx - 7, cy + k, 3, 2, alu);
        }
        fill(img, cx - 7, door_y, 4, 2, alu);
        // door: frame post at the bottom + the leaf swung open into the room
        fill(img, cx - 7, cy + ch - 1, 4, 2, alu);
        fill(img, cx - 7, door_y + 2, 1, ch - 3 - (door_y - cy), with_alpha(alu_d, 120));
        fill(img, cx - 2, door_y + 2, 2, 12, ff(shade(wood, 0.9)));
        fill(img, cx - 2, door_y + 2, 1, 12, ff(shade(wood, 1.25)));
        put(img, cx - 1, door_y + 12, ff(rgb(0xe8c547)));
        // right wall (the bookcase side)
        fill(img, cx + cw + 1, ry, 2, rh, ff(shade(wood, 0.4)));
        outline(img, rx, ry, rw, rh, ff(shade(wood, 0.42)));
        // executive chair: tall tufted leather back and arms (behind the CEO)
        let leather = rgb(0x4d3329);
        let ccx = cx + CEO_CX;
        let lt = |k: f32| ff(shade(leather, k));
        soft_shadow(img, furn, ccx - 9, cy + 31, 20, 2);
        fill(img, ccx - 7, cy + 6, 14, 1, lt(0.6));
        fill(img, ccx - 9, cy + 7, 18, 21, lt(0.6));
        fill(img, ccx - 8, cy + 7, 16, 20, lt(1.0));
        fill(img, ccx - 8, cy + 7, 16, 1, lt(1.5));
        fill(img, ccx - 8, cy + 7, 1, 20, lt(1.4));
        fill(img, ccx + 7, cy + 8, 1, 19, lt(0.75));
        for (kx, ky) in [(-4, 10), (0, 10), (4, 10), (-4, 16), (0, 16), (4, 16), (-4, 22), (0, 22), (4, 22)] {
            put(img, ccx + kx, cy + ky, lt(0.6));
            put(img, ccx + kx - 1, cy + ky - 1, lt(1.35));
        }
        // armrests + chrome posts
        for ax in [ccx - 12, ccx + 9] {
            fill(img, ax, cy + 22, 4, 3, lt(0.6));
            fill(img, ax + 1, cy + 22, 2, 2, lt(1.2));
            fill(img, ax + 1, cy + 25, 2, 6, ff(rgb(0x9aa3ad)));
        }
        // visitor chairs in front of the desk (seen from behind)
        let mut chair = procedural::chair_back(&furn.colors);
        if !furn.fx.is_identity() {
            canvas::map_pixels(&mut chair, &|c| furn.fx.apply(c));
        }
        for vx in [cx + 29, cx + 44] {
            soft_shadow(img, furn, vx + 1, cy + 63, 12, 1);
            blit(img, &chair, vx, cy + 51);
        }
    }

    // ---------------------------------------------------------------- dynamic layer

    fn draw_dynamic(&mut self, img: &mut RgbaImage, scene: &Scene, lay: &Layout, selected: Option<&AgentId>, t: u64) -> bool {
        let furn_t = self.th(Layer::Furniture);
        let decor_t = self.th(Layer::Decor);
        let char_t = self.th(Layer::Characters);
        let night = self.is_night(t);
        let mut hits: Vec<([i32; 4], AgentId)> = Vec::new();

        // Live Kanban sticky notes on the back-wall whiteboard
        let wb = decor_t.sprite("whiteboard");
        let wb_x = lay.w / 2 - wb.w() / 2;
        let wb_y = (WALL_H - wb.h()).max(2) / 2 + 2;
        if self.show.ambient && decor_t.fx.palette.is_empty() && wb.w() >= 28 && !scene.agents.is_empty() {
            let active_n = scene
                .agents
                .values()
                .filter(|a| matches!(a.state, AgentState::Typing | AgentState::Reading | AgentState::Running | AgentState::Thinking | AgentState::Delegating))
                .count()
                .min(4);
            let alert_n = scene.agents.values().filter(|a| matches!(a.state, AgentState::NeedsYou | AgentState::Error)).count().min(3);
            let done_n = scene.agents.values().filter(|a| a.state == AgentState::Done).count().min(3);
            for i in 0..active_n as i32 {
                fill(img, wb_x + 4 + i * 3, wb_y + 16, 2, 2, rgb(0x5ea8ff));
            }
            for i in 0..alert_n as i32 {
                fill(img, wb_x + 17 + i * 3, wb_y + 16, 2, 2, rgb(0xff5050));
            }
            for i in 0..done_n as i32 {
                fill(img, wb_x + 27 + i * 2, wb_y + 16, 2, 2, rgb(0x4cc46a));
            }
        }

        // council round on the meeting-room wall screen
        if let Some(round) = office_core::rosso::council_round(scene) {
            for pod in lay.pods.iter().filter(|p| p.kind == layout::PodKind::Council) {
                extras::draw_council_steps(img, furn_t, pod.x + POD_W / 2 - 21, pod.y, round.reached, round.current(), t);
            }
        }

        // Animated coffee bar steam
        let cooler = furn_t.sprite("cooler");
        let plant = decor_t.sprite("plant");
        let sofa = furn_t.sprite("sofa");
        let cooler_x = 4 + plant.w() + 8;
        let sofa_x = lay.w - layout::SOFA_CX_FROM_RIGHT - sofa.w() / 2;
        let cb_x = lay.coffee_bar.0 - 11;
        if self.show.ambient && cb_x > cooler_x + cooler.w() + 6 && cb_x + 32 < sofa_x {
            let strip_bottom = WALL_H + 26;
            let st_phase = ((t / 280) % 3) as i32;
            put(img, cb_x + 6, strip_bottom - 19 - st_phase, with_alpha(PAPER, 110));
            put(img, cb_x + 7, strip_bottom - 20 - (st_phase + 1) % 3, with_alpha(PAPER, 75));
        }

        // Animated server rack LEDs reflecting router status
        if let Some([_, cy, _, ch]) = lay.ceo.filter(|_| self.show.ambient) {
            let rx = lay.w - 20;
            let ry = cy + ch + 8;
            let bh = 10;
            if ry + 30 < lay.h - bh - plant.h() {
                let p = scene.router.pressure.unwrap_or(1);
                let led = if scene.router.queued > 0 || p >= 4 {
                    rgb(0xff5050)
                } else if p >= 2 || scene.router.busy_slots > 0 {
                    rgb(0xffd23f)
                } else {
                    rgb(0x4cc46a)
                };
                if scene.router_extra.orphans > 0 && (t / 300) % 2 == 0 {
                    fill(img, rx + 9, ry + 1, 2, 2, furn_t.fx.apply(rgb(0xff2a2a)));
                }
                for bay in 0..4 {
                    let on = ((t / 350) as i32 + bay) % 2 == 0;
                    if on {
                        put(img, rx + 9, ry + 4 + bay * 5, furn_t.fx.apply(led));
                    }
                    put(img, rx + 10, ry + 4 + bay * 5, furn_t.fx.apply(rgb(0x4dd0e1)));
                }
            }
        }

        // queue order for Queued agents
        let mut queued: Vec<&Agent> = scene.agents.values().filter(|a| a.state == AgentState::Queued).collect();
        queued.sort_by_key(|a| {
            (
                scene.queue_tickets.get(&a.id).map(|q| (q.pos, q.since_ms)).unwrap_or((u32::MAX, a.started_ms)),
                a.started_ms,
                a.id.clone(),
            )
        });
        let queue_pos: HashMap<&AgentId, usize> = queued.into_iter().enumerate().map(|(i, a)| (&a.id, i)).collect();
        let trips: HashMap<&AgentId, BossTrip> =
            scene.agents.values().filter_map(|a| self.boss_trip(a, scene, lay, t).map(|tr| (&a.id, tr))).collect();
        let places: HashMap<&AgentId, Place> = scene
            .agents
            .values()
            .map(|a| {
                let qi = queue_pos.get(&a.id).copied().unwrap_or(0);
                let base = place_with(a, lay, t, qi, self.show.ambient);
                let p = match trips.get(&a.id) {
                    Some(tr) if !matches!(base, Place::Hidden) => tr.place,
                    _ => base,
                };
                (&a.id, p)
            })
            .collect();

        // Free-standing agents (walking, queued, lounge stroll) sorted by y so we can 2.5D depth-interleave them with desk rows!
        let mut free: Vec<(&Agent, i32, i32, Option<u8>)> = scene
            .agents
            .values()
            .filter(|a| !trips.contains_key(&a.id))
            .filter_map(|a| match places.get(&a.id) {
                Some(Place::Free { fx, fy, walk }) => Some((a, *fx, *fy, *walk)),
                _ => None,
            })
            .collect();
        free.sort_by_key(|f| (f.2, f.1));
        let mut free_idx = 0usize;

        // desks row by row (so lower rows overlap upper ones, and free agents behind a desk row are drawn behind the desks)
        let mut cells: Vec<(i32, i32, Option<&Agent>)> = Vec::new();
        // council seats: cell -> (seat index 0..4, pod origin)
        let mut council_cells: HashMap<(i32, i32), (usize, i32, i32)> = HashMap::new();
        for pod in &lay.pods {
            for d in 0..layout::DESKS_PER_POD {
                let (x, y) = pod.cell(d);
                cells.push((x, y, pod.desks[d].as_ref().and_then(|id| scene.agents.get(id))));
                if pod.kind == layout::PodKind::Council {
                    council_cells.insert((x, y), (d, pod.x, pod.y));
                }
            }
        }
        cells.sort_by_key(|c| (c.1, c.0));
        let desk = furn_t.sprite("desk");
        let lamp = decor_t.sprite("lamp");
        let (council_table, chair_back) = if council_cells.is_empty() {
            (RgbaImage::new(1, 1), None)
        } else {
            let mut tb = procedural::council_table(&furn_t.colors);
            let mut cb = procedural::chair_back(&furn_t.colors);
            if !furn_t.fx.is_identity() {
                canvas::map_pixels(&mut tb, &|c| furn_t.fx.apply(c));
                canvas::map_pixels(&mut cb, &|c| furn_t.fx.apply(c));
            }
            (tb, Some(cb))
        };
        let mut idx = 0usize;
        while idx < cells.len() {
            let row_y = cells[idx].1;
            let row_desk_bottom = row_y + 48;
            while free_idx < free.len() && free[free_idx].2 < row_desk_bottom {
                let (a, fx, fy, walk) = free[free_idx];
                self.draw_free_agent(img, char_t, a, fx, fy, walk, t, selected, &mut hits);
                free_idx += 1;
            }
            while idx < cells.len() && cells[idx].1 == row_y {
                let (ox, oy, ag) = cells[idx];
                idx += 1;
                let at_desk = ag.and_then(|a| match places.get(&a.id) {
                    Some(Place::Desk { standing }) => Some((a, *standing)),
                    _ => None,
                });
                let council = council_cells.get(&(ox, oy)).copied();
                let near_side = council.is_some_and(|(d, _, _)| d >= 2);
                if let Some((a, standing)) = at_desk {
                    let pose = pose_for(a, t, true);
                    let cy = if standing { oy + 9 } else { oy + 13 };
                    let hl = selected == Some(&a.id) || self.hover.as_deref() == Some(a.id.as_str());
                    if near_side {
                        draw_char_back(img, char_t, a, &pose, ox + 18, cy, hl);
                    } else {
                        draw_char(img, char_t, a, &pose, ox + 18, cy, hl);
                    }
                    if a.state == AgentState::Done && t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS {
                        draw_confetti(img, char_t, ox + 26, cy, t);
                    }
                    hits.push(([ox + 4, oy + 4, CELL_W - 8, CELL_H - 12], a.id.clone()));
                }
                if let Some((d, px, py)) = council {
                    let [tx, ty, _, _] = layout::COUNCIL_TABLE;
                    // the table covers the far-side members' lap; near-side members sit in front of it
                    if !near_side {
                        blit(img, &council_table, px + tx, py + ty);
                    }
                    draw_council_seat(img, furn_t, chair_back.as_ref(), (ox, oy, d), at_desk.map(|(a, _)| a), t);
                    continue;
                }
                let desk_top = oy + 48 - desk.h();
                let narrow = desk.w() < 28;
                let (desk_left, desk_w) = if narrow { (ox + 24 - desk.w(), desk.w() * 2) } else { (ox + 24 - desk.w() / 2, desk.w()) };
                contact_shadow(img, furn_t, desk_left + 1, oy + 47, desk_w - 2, 2);
                if narrow {
                    blit(img, desk.frame(0), desk_left, desk_top);
                    blit(img, desk.frame(0), desk_left + desk.w(), desk_top);
                } else {
                    blit(img, desk.frame(0), desk_left, desk_top);
                }
                // monitor (anchored cleanly inside desk bounds even for 16px narrow tiles)
                let on_state = at_desk.map(|(a, _)| a.state);
                let mon = furn_t.sprite(if on_state.is_some() { "monitor_on" } else { "monitor_off" });
                let mx = (desk_left + 3).max(ox + 13 - mon.w() / 2).min(desk_left + desk_w - mon.w() - 8);
                let my = desk_top + 4 - mon.h();
                blit(img, mon.frame((t / 500) as usize), mx, my);
                // keyboard, mouse and a mug on the desk top (lit from the top-left)
                {
                    let ff = |c: u32| furn_t.fx.apply(rgb(c));
                    let kx = mx + mon.w() / 2 - 5;
                    let ky = desk_top + 4;
                    fill(img, kx, ky, 11, 2, ff(0x6b727c));
                    fill(img, kx, ky, 11, 1, ff(0xd3d7de));
                    for k in 0..4 {
                        put(img, kx + 1 + k * 3, ky, ff(0x8b929c));
                    }
                    fill(img, kx + 13, ky, 2, 2, ff(0x474d56));
                    put(img, kx + 13, ky, ff(0xe4e7ec));
                    let (ux, uy) = (desk_left + desk_w - 17, desk_top + 3);
                    fill(img, ux, uy, 3, 3, ff(0xf2f2f2));
                    fill(img, ux, uy, 3, 1, ff(0x5a3a22));
                    put(img, ux + 3, uy + 1, ff(0xd8d8d8));
                    put(img, ux + 2, uy + 2, ff(0xb4b4b4));
                }
                if let Some(st) = on_state {
                    let sr = mon.screen.unwrap_or([2, 2, mon.w() - 4, mon.h() - 6]);
                    let scr = [mx + sr[0], my + sr[1], sr[2], sr[3]];
                    screen_fx(img, furn_t, st, scr, t, at_desk.map(|(a, _)| character::hash(&a.id)).unwrap_or(0));
                    if furn_t.fx.palette.is_empty() {
                        if let Some((gc, rad, k)) = tool_glow_color(st) {
                            let boost = if furn_t.glow.is_some() { 1.35 } else { 1.0 };
                            glow(img, scr, furn_t.fx.apply(gc), rad, (k * boost).min(0.65));
                        } else if let Some(g) = furn_t.glow {
                            glow(img, scr, g, 5, 0.3);
                        }
                    }
                }
                // lamp (anchored within desk bounds; warm glow at night + blinking alert on NeedsYou)
                let lx = (ox + 40 - lamp.w() / 2).min(desk_left + desk_w - lamp.w() - 1);
                let ly = desk_top + 4 - lamp.h();
                blit(img, lamp.frame(0), lx, ly);
                if on_state == Some(AgentState::NeedsYou) && (t / 350) % 2 == 0 {
                    let c = decor_t.fx.apply(rgb(0xffd23f));
                    glow(img, [lx + lamp.w() / 2 - 2, ly, 4, 3], c, 6, 0.55);
                    fill(img, lx + lamp.w() / 2 - 2, ly + lamp.h() / 3, 4, 2, c);
                } else if night && on_state.is_some() && decor_t.fx.palette.is_empty() {
                    glow(img, [lx + lamp.w() / 2 - 1, ly + 1, 3, 3], rgb(0xffc966), 5, 0.16);
                }
                if let Some(n) = at_desk.and_then(|(a, _)| scene.in_trays.get(&a.id)).filter(|n| **n > 0) {
                    extras::draw_in_tray(img, furn_t, desk_left + desk_w - 11, desk_top + 3, *n);
                }
                if let Some((a, _)) = at_desk.filter(|(a, _)| a.kind == AgentKind::Claude && a.parent.is_none()) {
                    if let Some(mode) = office_core::rosso::dispatch_mode(scene, &a.id) {
                        extras::draw_dispatch_tag(img, furn_t, desk_left + desk_w / 2, oy + 41, mode);
                    }
                }
            }
        }
        while free_idx < free.len() {
            let (a, fx, fy, walk) = free[free_idx];
            self.draw_free_agent(img, char_t, a, fx, fy, walk, t, selected, &mut hits);
            free_idx += 1;
        }

        // the CEO in the corner office
        let mut ceo_head: Option<(i32, i32, ceo::Mood)> = None;
        if let Some(r) = lay.ceo {
            let hl = selected == Some(&CEO_ID.to_string()) || self.hover.as_deref() == Some(CEO_ID);
            let (top, mood) = self.draw_ceo(img, scene, lay, r, t, hl);
            hits.push(([r[0] + CEO_CX - 10, top - 2, 20, r[1] + layout::CEO_DESK_TOP - top + 2], CEO_ID.to_string()));
            ceo_head = Some((r[0] + CEO_CX, top, mood));
        }
        // sessions visiting the boss (drawn after the CEO's desk so they stand in front of it)
        let mut voting = false;
        for (id, tr) in &trips {
            let Some(a) = scene.agents.get(*id) else { continue };
            if let Place::Free { fx, fy, walk } = places[*id] {
                self.draw_free_agent(img, char_t, a, fx, fy, walk, t, selected, &mut hits);
                // the document: a plan held up for a vote, or the turn's result
                let (px, py) = if tr.at_boss { (fx + 3, fy - 19) } else { (fx + 4, fy - 13) };
                draw_doc(img, char_t, px, py, if tr.approval { DocKind::Plan } else { DocKind::Result });
                voting |= tr.approval && tr.at_boss;
            }
        }
        let mut reserved: Vec<[i32; 4]> = Vec::new();
        if voting {
            if let Some([cx, cy, _, _]) = lay.ceo {
                let (vx, vy) = (cx + 6, cy + 12);
                draw_vote(img, char_t, vx, vy, t);
                reserved.push([vx - 2, vy - font::GLYPH_H - 2, 24, font::GLYPH_H + 12]);
            }
        }
        // couriers: a postman carries each brief or report from the sender's desk to the recipient's
        let mut couriers = 0;
        for r in scene.reports.iter().rev().take(24).filter(|_| self.show.couriers) {
            if r.kind == office_core::ReportKind::Result || t < r.at_ms || t - r.at_ms > TRIP_MAX_MS {
                continue;
            }
            let Some(to) = r.to.as_deref().and_then(|to| desk_aisle(lay, to)) else { continue };
            let from = desk_aisle(lay, &r.from).unwrap_or(lay.door);
            if let Some(Place::Free { fx, fy, walk }) = walk_along_min(&lay.walk_route(from, to), r.at_ms, t, 2_800.0) {
                let doc = doc_kind_for(scene.report_types.get(&r.id).map(String::as_str), r.kind == office_core::ReportKind::Task);
                draw_courier(img, char_t, r.id, fx, fy, walk.unwrap_or(0), doc);
                couriers += 1;
            }
        }

        // the Security Desk guard by the door: raises a hand for a few seconds after a denial
        let mut guard: Option<(i32, i32, bool)> = None;
        if self.show.ambient {
            let g = &scene.guard;
            let alert = g.last_deny_ms > 0 && t >= g.last_deny_ms && t - g.last_deny_ms < extras::GUARD_ALERT_MS;
            extras::draw_guard(img, char_t, lay.door.0 + 24, lay.door.1, alert, t);
            guard = Some((lay.door.0 + 24, lay.door.1, alert));
        }

        // the office cat: above the floor and furniture, below the labels
        let mut cat_at: Option<cat::CatState> = None;
        if self.cat_on() {
            let st = self.cat_mem.borrow_mut().state(lay, scene, t);
            let hl = selected == Some(&CAT_ID.to_string()) || self.hover.as_deref() == Some(CAT_ID);
            contact_shadow(img, char_t, st.x - 4, st.y - 1, 8, 2);
            let mut buf = RgbaImage::new(cat::BOX_W as u32 + 8, cat::BOX_H as u32 + 4);
            cat::draw(&mut buf, 4 + cat::BOX_W / 2, 2 + cat::BOX_H, &st, &cat::coat(&self.cat.color));
            themed_sprite(&mut buf, char_t, hl);
            blit(img, &buf, st.x - cat::BOX_W / 2 - 4, st.y - cat::BOX_H - 2);
            hits.push(([st.x - cat::BOX_W / 2 - 1, st.y - cat::BOX_H - 1, cat::BOX_W + 2, cat::BOX_H + 2], CAT_ID.to_string()));
            cat_at = Some(st);
        }

        if self.show.ambient {
            let wave = office_core::rosso::latest_wave(scene);
            if let Some(aj) = office_core::rosso::latest_auto_job(scene).filter(|aj| aj.status == "running" || wave.is_none()) {
                let t_sec = ((t % 1_000_000) as f32) / 1000.0;
                let ar = extras::draw_auto_strip(img, furn_t, lay.w / 2, WALL_H + 2, aj, t_sec);
                reserved.push(ar);
            } else if let Some(w) = wave {
                let wr = extras::draw_wave_strip(img, furn_t, lay.w / 2, WALL_H + 2, w, t);
                reserved.push(wr);
            }
        }

        // overlays: priority-ordered rect collision resolver so no two badges or speech bubbles overlap
        self.draw_overlays(img, char_t, scene, lay, &places, selected, t, ceo_head, cat_at, &reserved, guard);
        self.hits = hits;
        couriers > 0
            || places.values().any(|p| matches!(p, Place::Free { walk: Some(_), .. }))
            || cat_at.as_ref().is_some_and(|st| matches!(st.action, cat::Action::Walk(_)))
    }

    /// A top-level session walking to the boss: to get a plan approved (it waits there until
    /// you answer), or to hand over a finished turn's result (it presents it, then walks back).
    fn boss_trip(&self, a: &Agent, scene: &Scene, lay: &Layout, t: u64) -> Option<BossTrip> {
        if !self.show.boss_visits {
            return None;
        }
        if a.kind != office_core::model::AgentKind::Claude || a.parent.is_some() {
            return None;
        }
        let spot = boss_spot(lay)?;
        let aisle = desk_aisle(lay, &a.id)?;
        let approval = a.state == AgentState::NeedsYou
            && matches!(a.current_tool.as_deref(), Some("AskUserQuestion" | "ExitPlanMode"));
        let mut mem = self.visit_mem.borrow_mut();
        if approval {
            let e = mem.entry(a.id.clone()).or_insert((t, None));
            if e.1.is_some() {
                *e = (t, None);
            }
            let start = e.0;
            return Some(match walk_along(&lay.walk_route(aisle, spot), start, t) {
                Some(p) => BossTrip { place: p, at_boss: false, approval: true },
                None => BossTrip { place: Place::Free { fx: spot.0, fy: spot.1, walk: None }, at_boss: true, approval: true },
            });
        }
        if let Some(e) = mem.get_mut(&a.id) {
            let end = *e.1.get_or_insert(t);
            match walk_along(&lay.walk_route(spot, aisle), end, t) {
                Some(p) if t.saturating_sub(end) < TRIP_MAX_MS => return Some(BossTrip { place: p, at_boss: false, approval: false }),
                _ => {
                    mem.remove(&a.id);
                }
            }
        }
        drop(mem);
        let r = scene.reports.iter().rev().find(|r| {
            r.kind == office_core::ReportKind::Result && r.from == a.id && t >= r.at_ms && t - r.at_ms < TRIP_MAX_MS
        })?;
        let there = lay.walk_route(aisle, spot);
        let arrive = r.at_ms + route_ms(&there);
        if let Some(p) = walk_along(&there, r.at_ms, t) {
            return Some(BossTrip { place: p, at_boss: false, approval: false });
        }
        if t < arrive + PRESENT_MS {
            return Some(BossTrip { place: Place::Free { fx: spot.0, fy: spot.1, walk: None }, at_boss: true, approval: false });
        }
        walk_along(&lay.walk_route(spot, aisle), arrive + PRESENT_MS, t).map(|p| BossTrip { place: p, at_boss: false, approval: false })
    }

    fn draw_free_agent(
        &self,
        img: &mut RgbaImage,
        char_t: &Theme,
        a: &Agent,
        fx: i32,
        fy: i32,
        walk: Option<u8>,
        t: u64,
        selected: Option<&AgentId>,
        hits: &mut Vec<([i32; 4], AgentId)>,
    ) {
        let mut pose = pose_for(a, t, false);
        if let Some(ph) = walk {
            pose.legs = Legs::Walk(ph);
            pose.arms = Arms::Swing(ph);
            pose.bob = 0;
        }
        contact_shadow(img, char_t, fx - 5, fy - 1, 10, 2);
        let hl = selected == Some(&a.id) || self.hover.as_deref() == Some(a.id.as_str());
        draw_char(img, char_t, a, &pose, fx - 8, fy - 24, hl);
        hits.push(([fx - 9, fy - 26, 18, 27], a.id.clone()));
    }

    fn draw_overlays(
        &self,
        img: &mut RgbaImage,
        char_t: &Theme,
        scene: &Scene,
        lay: &Layout,
        places: &HashMap<&AgentId, Place>,
        selected: Option<&AgentId>,
        t: u64,
        ceo_head: Option<(i32, i32, ceo::Mood)>,
        cat_at: Option<cat::CatState>,
        reserved: &[[i32; 4]],
        guard: Option<(i32, i32, bool)>,
    ) {
        let mut order: Vec<(&Agent, u8)> = scene
            .agents
            .values()
            .filter_map(|a| {
                let p = places.get(&a.id)?;
                if matches!(p, Place::Hidden) {
                    return None;
                }
                let is_sel = selected == Some(&a.id);
                let is_hov = self.hover.as_deref() == Some(a.id.as_str());
                let prio = if is_sel {
                    5
                } else if is_hov {
                    4
                } else if a.state == AgentState::NeedsYou {
                    3
                } else if a.state == AgentState::Error {
                    2
                } else if matches!(
                    a.state,
                    AgentState::Typing | AgentState::Running | AgentState::Reading | AgentState::Delegating | AgentState::Queued
                ) {
                    1
                } else {
                    0
                };
                Some((a, prio))
            })
            .collect();
        // Plan high-priority rects first so critical/selected overlays ALWAYS win space, then draw low-to-high.
        order.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.started_ms.cmp(&b.0.started_ms)).then(a.0.id.cmp(&b.0.id)));

        struct Planned<'a> {
            a: &'a Agent,
            cx: i32,
            head_y: i32,
            is_sel: bool,
            is_hov: bool,
            show_name: bool,
            name: String,
            name_w: i32,
            verdict: Option<&'a str>,
            queue: Option<&'a office_core::QueueTicket>,
            name_rect: [i32; 4],
            bubble: Option<(String, [i32; 4], Col, Col)>,
            icon: Option<(AgentState, i32, i32)>,
        }

        let mut placed: Vec<[i32; 4]> = Vec::new();
        let mut planned: Vec<Planned<'_>> = Vec::with_capacity(order.len());
        let f = |c: Col| char_t.fx.apply(c);
        let bounds = [3, WALL_H + 2, lay.w - 6, lay.h - WALL_H - 4];

        // Session nameplates live in each zone's reserved strip; labels and bubbles avoid them.
        for z in lay.zones.iter().filter(|z| z.head && self.show.nameplates) {
            let [zx, zy, zw, _] = z.rect;
            let members = lay.pods.iter().filter(|p| p.group == z.group).flat_map(|p| p.desks.iter().flatten()).count();
            let name = match z.session.as_ref().and_then(|id| scene.agents.get(id)) {
                Some(s) => s.label.clone(),
                None => "AGY LAB".to_string(),
            };
            let txt = if members > 1 { format!("{name} · {members}") } else { name };
            let tag = z
                .session
                .as_ref()
                .and_then(|id| scene.entrypoints.get(id))
                .map(|e| extras::entry_tag(e))
                .filter(|t| !t.is_empty());
            let tag_w = tag.map(|t| font::text_width(t) + 3 + 2).unwrap_or(0);
            let txt = font::truncate(&txt, zw - 6 - tag_w);
            let tw = font::text_width(&txt);
            if !self.show.zones {
                fill(img, zx, zy, tw + 6 + tag_w, layout::ZONE_HEAD - 1, with_alpha(f(INK), 200));
            }
            font::draw_text(img, zx + 3, zy + 2, &txt, f(PAPER));
            if let Some(tag) = tag {
                extras::draw_entry_tag(img, char_t, zx + 3 + tw + 3, zy + 1, tag);
            }
            placed.push([zx, zy, tw + 6 + tag_w, layout::ZONE_HEAD - 1]);
        }
        placed.extend_from_slice(reserved);
        // the guard's tag and hand bubble go through the same resolver (tag first, then the icon beside/above it)
        let mut guard_ov: Option<([i32; 4], Option<[i32; 4]>)> = None;
        if let Some((gx, gy, alert)) = guard {
            let fits = |r: &[i32; 4]| r[0] >= bounds[0] && r[1] >= bounds[1] && r[0] + r[2] <= bounds[0] + bounds[2] && r[1] + r[3] <= bounds[1] + bounds[3];
            let pick = |c: Vec<[i32; 4]>, placed: &[[i32; 4]]| {
                c.iter().copied().find(|r| fits(r) && !placed.iter().any(|p| rects_overlap(*r, *p))).or_else(|| c.iter().copied().find(fits))
            };
            if let Some(tag) = pick(extras::guard_tag_candidates(gx, gy), &placed) {
                placed.push(tag);
                let icon = if alert && self.show.icons { pick(extras::guard_icon_candidates(gx, gy), &placed) } else { None };
                if let Some(i) = icon {
                    placed.push(i);
                }
                guard_ov = Some((tag, icon));
            }
        }
        // council seats (member -> seat index, pod origin) and the conference tables/AGENDA screens labels must not cover
        let mut council_seat: HashMap<&AgentId, (usize, i32, i32)> = HashMap::new();
        let mut table_rects: Vec<[i32; 4]> = Vec::new();
        for pod in lay.pods.iter().filter(|p| p.kind == layout::PodKind::Council) {
            let [tx, ty, tw, th] = layout::COUNCIL_TABLE;
            table_rects.push([pod.x + tx, pod.y + ty, tw, th]);
            placed.push([pod.x + layout::POD_W / 2 - 22, pod.y + 3, 44, 10]);
            for (d, id) in pod.desks.iter().enumerate() {
                if let Some(id) = id {
                    council_seat.insert(id, (d, pod.x, pod.y));
                }
            }
        }
        // Nameplates, reserved strips, council AGENDA screens, and the boss's ballot are never covered, even by must-show overlays.
        let plates: Vec<[i32; 4]> = placed.clone();
        let best_spot = |cands: &[[i32; 4]], placed: &[[i32; 4]]| -> Option<[i32; 4]> {
            cands
                .iter()
                .copied()
                .filter(|r| !plates.iter().any(|p| rects_overlap(*r, *p)))
                .min_by_key(|r| placed.iter().filter(|p| rects_overlap(*r, **p)).count())
        };

        for (a, _prio) in &order {
            let is_sel = selected == Some(&a.id);
            let is_hov = self.hover.as_deref() == Some(a.id.as_str());
            let (cx, head_y) = match places[&a.id] {
                Place::Desk { standing } => {
                    let s = lay.seats[&a.id];
                    (s.x + 26, if standing { s.y + 10 } else { s.y + 14 })
                }
                Place::Free { fx, fy, .. } => (fx, fy - 24),
                Place::Hidden => continue,
            };
            let max_name = if is_sel || is_hov { 120 } else { CELL_W - 4 };
            let verdict = scene.verdicts.get(&a.id).map(String::as_str);
            let queue = (a.state == AgentState::Queued).then(|| scene.queue_tickets.get(&a.id)).flatten();
            // ROSSO titles abbreviate ("SEC SEAT") before they would be cut, and the verdict chip keeps its room
            let name_budget = if verdict.is_some() && !(is_sel || is_hov) {
                max_name + 12 - verdict.map(extras::chip_width).unwrap_or(0)
            } else {
                max_name
            };
            let name = extras::tag_name(&a.label, name_budget);
            let name_w = font::text_width(&name) + 5; // +2px pip +1px gap +2px pad
            let nw = name_w
                + verdict.map(extras::chip_width).unwrap_or(0)
                + queue.map(extras::queue_chip_width).unwrap_or(0);
            let nh = font::GLYPH_H + 2;
            let nx = (cx - nw / 2).clamp(bounds[0], (bounds[0] + bounds[2] - nw).max(bounds[0]));
            let ny = (head_y - 10).clamp(bounds[1], (bounds[1] + bounds[3] - nh).max(bounds[1]));
            let min_x = bounds[0];
            let max_x = (bounds[0] + bounds[2] - nw).max(min_x);
            let min_y = bounds[1];
            let max_y = (bounds[1] + bounds[3] - nh).max(min_y);
            let mut name_candidates: Vec<[i32; 4]> = vec![
                [nx, ny, nw, nh],
                [(nx + 16).min(max_x), ny, nw, nh],
                [(nx - 16).max(min_x), ny, nw, nh],
                [(nx + 28).min(max_x), ny, nw, nh],
                [(nx - 28).max(min_x), ny, nw, nh],
                [(nx + 40).min(max_x), ny, nw, nh],
                [(nx - 40).max(min_x), ny, nw, nh],
                [nx, (ny - nh - 2).max(min_y), nw, nh],
                [nx, (ny - (nh + 2) * 2).max(min_y), nw, nh],
                [nx, (ny + nh + 2).min(max_y), nw, nh],
                [nx, (head_y + 26).min(max_y), nw, nh],
                [nx, (ny - (nh + 2) * 3).max(min_y), nw, nh],
            ];
            // Council members: far side labels sit above the heads, near side labels below the chair
            // (under the glass front wall, then inside the cabin behind the chair); never over the table.
            if let Some(&(d, _, pod_y)) = council_seat.get(&a.id) {
                let mut c: Vec<[i32; 4]> = Vec::new();
                if d >= 2 {
                    let by = (pod_y + layout::POD_H - 1).clamp(min_y, max_y);
                    for dx in [0, -14, 14, -28, 28] {
                        c.push([(nx + dx).clamp(min_x, max_x), by, nw, nh]);
                    }
                    let iy = (pod_y + layout::COUNCIL_ROW0 + layout::COUNCIL_ROW_DY + 42).clamp(min_y, max_y);
                    c.push([nx, iy, nw, nh]);
                } else {
                    c.push([nx, ny, nw, nh]);
                    for dx in [-16, 16, -28, 28] {
                        c.push([(nx + dx).clamp(min_x, max_x), ny, nw, nh]);
                    }
                    for k in 1..=2 {
                        c.push([nx, (ny - (nh + 2) * k).max(min_y), nw, nh]);
                    }
                }
                let clear: Vec<[i32; 4]> = c.iter().copied().filter(|r| !table_rects.iter().any(|t| rects_overlap(*r, *t))).collect();
                name_candidates = if clear.is_empty() { c } else { clear };
            }
            let name_rect = name_candidates
                .iter()
                .copied()
                .find(|r| !placed.iter().any(|p| rects_overlap(*r, *p)))
                .or_else(|| best_spot(&name_candidates, &placed))
                .unwrap_or(name_candidates[0]);
            let focus = is_sel || is_hov || matches!(a.state, AgentState::NeedsYou | AgentState::Error);
            let show_name = self.show.names && self.label_gate(focus);
            if show_name {
                placed.push(name_rect);
            }
            let ny = name_rect[1];

            let speaking = matches!(
                a.state,
                AgentState::Thinking
                    | AgentState::Typing
                    | AgentState::Reading
                    | AgentState::Running
                    | AgentState::Delegating
                    | AgentState::NeedsYou
                    | AgentState::Error
                    | AgentState::Queued
            );
            let fail = scene.failures.get(&a.id).filter(|fi| t.saturating_sub(fi.at_ms) < FAILURE_SHOW_MS && !fi.hint.is_empty());
            let must_show = is_sel || is_hov || fail.is_some() || matches!(a.state, AgentState::NeedsYou | AgentState::Error);
            let mut bubble_op = None;
            if (speaking && !a.activity.is_empty() || fail.is_some()) && self.show.bubbles && (fail.is_some() || self.label_gate(focus)) {
                let comp = match fail {
                    Some(fi) => extras::compact_hint(&fi.hint),
                    None => compact_activity(&a.activity),
                };
                let max_b = if is_sel || is_hov || fail.is_some() { 124 } else { CELL_W - 4 };
                let text = font::truncate_words(&comp, max_b);
                let bw = font::text_width(&text) + 4;
                let bh = font::GLYPH_H + 4;
                let base_y = (ny - bh - 2).clamp(bounds[1], (bounds[1] + bounds[3] - bh).max(bounds[1]));
                let base_x = (cx - bw / 2).clamp(bounds[0], (bounds[0] + bounds[2] - bw).max(bounds[0]));
                let recent_fail = a.last_tool_ok == Some(false) && t.saturating_sub(a.last_event_ms) < 2_500;
                let (bfg, bbg) = match a.state {
                    AgentState::NeedsYou => (f(INK), f(rgb(0xffd23f))),
                    AgentState::Error => (f(PAPER), f(rgb(0xe03c3c))),
                    _ if fail.is_some() => (f(PAPER), f(rgb(0xe03c3c))),
                    _ if recent_fail => (f(PAPER), f(rgb(0xe03c3c))),
                    _ if is_sel || is_hov => (f(INK), f(PAPER)),
                    _ => (f(PAPER), with_alpha(f(rgb(0x1f2430)), 220)),
                };
                {
                    let max_x = (bounds[0] + bounds[2] - bw).max(bounds[0]);
                    let mut candidates = Vec::with_capacity(35);
                    // near-side council members face away: their bubble stacks below the label instead
                    let near_council = council_seat.get(&a.id).is_some_and(|c| c.0 >= 2);
                    let max_y = (bounds[1] + bounds[3] - bh).max(bounds[1]);
                    for dy in [0, 1, 2, 3, 4].map(|k| k * (bh + OVERLAY_GAP)) {
                        let y = if near_council { (ny + nh + OVERLAY_GAP + dy).min(max_y) } else { (base_y - dy).max(bounds[1]) };
                        for dx in [0, -12, 12, -24, 24, -36, 36] {
                            candidates.push([(base_x + dx).clamp(bounds[0], max_x), y, bw, bh]);
                        }
                    }
                    if council_seat.contains_key(&a.id) {
                        let clear: Vec<[i32; 4]> =
                            candidates.iter().copied().filter(|r| !table_rects.iter().any(|t| rects_overlap(*r, *t))).collect();
                        if !clear.is_empty() {
                            candidates = clear;
                        }
                    }
                    let free = candidates.iter().copied().find(|r| !placed.iter().any(|p| rects_overlap(*r, *p)));
                    let fallback = if must_show { best_spot(&candidates, &placed).or(Some(candidates[0])) } else { None };
                    if let Some(rect) = free.or(fallback) {
                        placed.push(rect);
                        bubble_op = Some((text, rect, bfg, bbg));
                    }
                }
            }

            let icon = match a.state {
                _ if fail.is_some() => Some(AgentState::Error),
                AgentState::Thinking | AgentState::NeedsYou | AgentState::Error | AgentState::Queued => Some(a.state),
                AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS * 2 => Some(AgentState::Done),
                _ => None,
            }
            .filter(|_| self.show.icons)
            .map(|k| (k, cx + 9, head_y - 1));
            if let Some((_, ix, iy)) = icon {
                placed.push([ix, iy, 9, 11]);
            }

            planned.push(Planned { a, cx, head_y, is_sel, is_hov, show_name, name, name_w, verdict, queue, name_rect, bubble: bubble_op, icon });
        }

        for p in planned.into_iter().rev() {
            let [nx, ny, nw, nh] = p.name_rect;
            if !p.show_name {
            } else if p.is_sel {
                fill(img, nx, ny, nw, nh, f(SELECT));
                fill(img, nx, ny, 2, nh, f(kind_color(p.a.kind)));
                font::draw_text(img, nx + 3, ny + 1, &p.name, f(INK));
            } else {
                fill(img, nx, ny, nw, nh, with_alpha(f(INK), 224));
                if p.is_hov {
                    outline(img, nx - 1, ny - 1, nw + 2, nh + 2, f(PAPER));
                }
                fill(img, nx, ny, 2, nh, f(kind_color(p.a.kind)));
                font::draw_text(img, nx + 3, ny + 1, &p.name, f(PAPER));
            }
            let mut chip_x = nx + p.name_w;
            if let (true, Some(v)) = (p.show_name, p.verdict) {
                extras::draw_chip(img, char_t, chip_x, ny, nh, v, t);
                chip_x += extras::chip_width(v);
            }
            if let (true, Some(qt)) = (p.show_name, p.queue) {
                extras::draw_queue_chip(img, char_t, chip_x, ny, nh, qt);
            }
            if let Some((text, [bx, by, bw, bh], fg, bg)) = p.bubble {
                let border = f(INK);
                fill(img, bx, by, bw, bh, bg);
                outline(img, bx, by, bw, bh, border);
                let tail_x = p.cx.clamp(bx + 2, bx + bw - 3);
                put(img, tail_x, by + bh, border);
                put(img, tail_x - 1, by + bh, bg);
                put(img, tail_x, by + bh + 1, border);
                font::draw_text(img, bx + 2, by + 2, &text, fg);
            }
            if let Some((k, ix, iy)) = p.icon {
                icon_bubble(img, char_t, ix, iy, k, t);
            }
            let _ = p.head_y;
        }

        if let Some((tag, icon)) = guard_ov {
            extras::draw_guard_tag(img, char_t, tag);
            if let Some([ix, iy, _, _]) = icon {
                extras::draw_hand_bubble(img, char_t, ix, iy, t);
            }
        }

        if let Some((hx, top, mood)) = ceo_head {
            if self.show.icons && matches!(mood, ceo::Mood::Alert { .. }) {
                icon_bubble(img, char_t, hx + 6, top - 9, AgentState::NeedsYou, t);
            }
        }
        if let Some(st) = cat_at {
            let top = st.y - if st.action == cat::Action::Sleep { 6 } else { cat::BOX_H };
            let on_desk = matches!(
                lay.spots.iter().find(|s| s.x == st.x && s.y == st.y).map(|s| s.kind),
                Some(layout::SpotKind::Desk(..))
            );
            let named = selected == Some(&CAT_ID.to_string()) || self.hover.as_deref() == Some(CAT_ID);
            let mut name_w = 0;
            if named && !self.cat.name.is_empty() {
                let name = font::truncate(&self.cat.name, 80);
                name_w = font::text_width(&name);
                let lx = if on_desk { st.x + 10 } else { st.x };
                let ly = if on_desk { top - 2 } else { top - 8 };
                font::label(img, lx.clamp(20, lay.w - 20), ly.max(WALL_H + 2), &name, f(INK), f(SELECT));
            }
            if st.action == cat::Action::Sleep {
                let txt = ["z", "zZ", "zZz"][((t / 700) % 3) as usize];
                let dy = ((t / 350) % 2) as i32;
                let x0 = if on_desk { st.x + 8 } else { st.x + 3 + name_w / 2 };
                let ly = if on_desk { top - 2 - dy } else { top - 7 - dy };
                font::label(img, x0 + font::text_width(txt) / 2, ly.max(WALL_H + 2), txt, f(PAPER), with_alpha(f(INK), 150));
            }
        }

        let _ = &placed;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocKind {
    /// A brief handed down (white envelope, red seal).
    Task,
    /// A report handed back (yellow folder).
    Report,
    /// A plan waiting for the boss's vote (white sheet with lines and a "?").
    Plan,
    /// A finished turn handed to the boss (blue folder).
    Result,
    /// A council critique (red folder).
    Critique,
    /// A rebuttal (orange folder).
    Rebuttal,
    /// A decision record (paper with a stamp).
    Decision,
    /// A sign-off sheet (paper with a green check).
    Signoff,
    /// An inspection report (clipboard).
    Inspection,
}

/// Document icon for a ROSSO document type (falls back to the plain task/report art).
fn doc_kind_for(doc_type: Option<&str>, task: bool) -> DocKind {
    match doc_type {
        Some("Critique") => DocKind::Critique,
        Some("Rebuttal") => DocKind::Rebuttal,
        Some("Decision Record") => DocKind::Decision,
        Some("Sign-off Sheet") => DocKind::Signoff,
        Some("Inspection Report") => DocKind::Inspection,
        _ if task => DocKind::Task,
        _ => DocKind::Report,
    }
}

fn draw_doc(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, k: DocKind) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let ink = f(0x2a2a33);
    match k {
        DocKind::Task => {
            fill(img, x, y, 7, 5, f(0xf6f3ea));
            outline(img, x, y, 7, 5, ink);
            put(img, x + 1, y + 1, ink);
            put(img, x + 5, y + 1, ink);
            put(img, x + 2, y + 2, ink);
            put(img, x + 4, y + 2, ink);
            put(img, x + 3, y + 3, f(0xd23c3c));
        }
        DocKind::Report => {
            fill(img, x, y, 7, 6, f(0xf2c94c));
            outline(img, x, y, 7, 6, f(0x8a6a1c));
            fill(img, x + 1, y + 2, 5, 1, f(0xfff3c4));
        }
        DocKind::Plan => {
            fill(img, x, y, 8, 10, f(0xfbfaf4));
            outline(img, x, y, 8, 10, ink);
            for k in [2, 4, 6] {
                fill(img, x + 2, y + k, 4, 1, f(0x8a8f9c));
            }
            put(img, x + 6, y + 8, f(0xd23c3c));
        }
        DocKind::Result => {
            fill(img, x, y, 7, 6, f(0x4aa3df));
            outline(img, x, y, 7, 6, f(0x1d5a85));
            fill(img, x + 1, y + 2, 5, 1, f(0xd8f0ff));
        }
        DocKind::Critique | DocKind::Rebuttal => {
            let (body, edge, tab) = if k == DocKind::Critique { (0xd23c3c, 0x7a1f1f, 0xf08a8a) } else { (0xf29a3a, 0x8a4f12, 0xffd199) };
            fill(img, x, y, 7, 6, f(body));
            outline(img, x, y, 7, 6, f(edge));
            fill(img, x, y - 1, 3, 1, f(edge));
            fill(img, x + 1, y + 2, 5, 1, f(tab));
            fill(img, x + 1, y + 4, 3, 1, f(tab));
        }
        DocKind::Decision => {
            fill(img, x, y, 7, 8, f(0xfbfaf4));
            outline(img, x, y, 7, 8, ink);
            fill(img, x + 1, y + 1, 4, 1, f(0x8a8f9c));
            fill(img, x + 1, y + 3, 5, 1, f(0x8a8f9c));
            // round red stamp
            fill(img, x + 3, y + 4, 3, 3, f(0xc02a2a));
            put(img, x + 4, y + 5, f(0xfbfaf4));
        }
        DocKind::Signoff => {
            fill(img, x, y, 7, 8, f(0xfbfaf4));
            outline(img, x, y, 7, 8, ink);
            fill(img, x + 1, y + 1, 4, 1, f(0x8a8f9c));
            for (dx, dy) in [(1, 5), (2, 6), (3, 5), (4, 4), (5, 3)] {
                put(img, x + dx, y + dy, f(0x2fae5e));
            }
        }
        DocKind::Inspection => {
            fill(img, x, y, 7, 9, f(0x8a5a36));
            fill(img, x + 1, y + 1, 5, 7, f(0xfbfaf4));
            fill(img, x + 2, y - 1, 3, 2, f(0xb9bec6));
            for ky in [3, 5, 7] {
                put(img, x + 2, y + ky, f(0x2fae5e));
                fill(img, x + 3, y + ky, 2, 1, f(0x8a8f9c));
            }
        }
    }
}

/// The boss's ballot: approve (green check) or request changes (red cross), blinking in turn.
fn draw_vote(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, t: u64) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let on = (t / 450) % 2 == 0;
    let (ok_bg, no_bg) = if on { (f(0x3fae6a), f(0x5a2a2a)) } else { (f(0x24533a), f(0xd04040)) };
    fill(img, x, y, 9, 9, ok_bg);
    outline(img, x, y, 9, 9, f(0x1b1b22));
    for (dx, dy) in [(2, 4), (3, 5), (4, 6), (5, 5), (6, 4), (7, 3)] {
        put(img, x + dx, y + dy, f(0xffffff));
    }
    fill(img, x + 11, y, 9, 9, no_bg);
    outline(img, x + 11, y, 9, 9, f(0x1b1b22));
    for d in 2..7 {
        put(img, x + 11 + d, y + d, f(0xffffff));
        put(img, x + 11 + 8 - d, y + d, f(0xffffff));
    }
    font::draw_text(img, x - 1, y - font::GLYPH_H - 1, "VOTE", f(0xffd23f));
}

/// The office postman: a navy-uniformed courier with a cap and a satchel, carrying a document.
fn draw_courier(img: &mut RgbaImage, th: &Theme, seed: u64, fx: i32, fy: i32, phase: u8, doc: DocKind) {
    let mut look = character::look_for(&format!("courier-{seed}"));
    look.hair = rgb(0x22335a); // cap
    look.hair_style = 0;
    look.shirt = rgb(0x2f5fa8);
    look.pants = rgb(0x22335a);
    look.glasses = false;
    let pose = Pose { arms: Arms::Swing(phase), legs: Legs::Walk(phase), ..Default::default() };
    contact_shadow(img, th, fx - 5, fy - 1, 10, 2);
    let mut buf = RgbaImage::new(character::CW as u32 + 4, character::CH as u32 + 8);
    character::draw(&mut buf, 2, 6, &look, &pose);
    themed_sprite(&mut buf, th, false);
    blit(img, &buf, fx - 10, fy - 30);
    // cap brim + satchel strap and bag
    let f = |c: u32| th.fx.apply(rgb(c));
    fill(img, fx - 5, fy - 23, 11, 1, f(0x22335a));
    put(img, fx - 3, fy - 14, f(0x6b4a2b));
    put(img, fx - 2, fy - 13, f(0x6b4a2b));
    fill(img, fx - 9, fy - 10, 4, 4, f(0x8a5a36));
    draw_doc(img, th, fx + 4, fy - 13, doc);
}

/// Outdoor scene behind a back-wall window: sky (dusk-blue with stars at night), an optional
/// `window_ground` strip, an optional far `window_view_far` (roofs) and the `window_view` sprite,
/// all clipped to the glass rect, then a mullion. Night multiplies everything dark blue.
fn draw_window_view(img: &mut RgbaImage, th: &Theme, view: &theme::Sprite, glass: [i32; 4], night: bool, seed: i32) {
    let [gx, gy, gw, gh] = glass;
    if gw < 6 || gh < 6 {
        return;
    }
    let f = |c: Col| th.fx.apply(c);
    let night_mul = |c: Col| if night { [(c[0] as u16 * 70 / 255) as u8, (c[1] as u16 * 85 / 255) as u8, (c[2] as u16 * 140 / 255) as u8, c[3]] } else { c };
    let (top, bot) = if night || !th.fx.palette.is_empty() { (rgb(0x0b1230), rgb(0x1d2a52)) } else { (rgb(0x7fc4f0), rgb(0xcdeeff)) };
    for yy in 0..gh {
        fill(img, gx, gy + yy, gw, 1, f(procedural::mix(top, bot, yy as f32 / (gh - 1) as f32)));
    }
    if night {
        for k in 0..4 {
            let sx = gx + 2 + (seed * 7 + k * 11).rem_euclid((gw - 4).max(1));
            let sy = gy + 1 + (seed * 3 + k * 5).rem_euclid((gh / 2).max(1));
            put(img, sx, sy, f(rgb(0xf4f1d0)));
        }
    }
    // clipped blit: only pixels inside the glass rect
    let clip = |img: &mut RgbaImage, s: &RgbaImage, ox: i32, oy: i32| {
        for sy in 0..s.height() as i32 {
            for sx in 0..s.width() as i32 {
                let (x, y) = (ox + sx, oy + sy);
                if x < gx || y < gy || x >= gx + gw || y >= gy + gh {
                    continue;
                }
                let p = s.get_pixel(sx as u32, sy as u32).0;
                if p[3] > 0 {
                    put(img, x, y, f(night_mul([p[0], p[1], p[2], 255])));
                }
            }
        }
    };
    let ground_h = 4;
    let gtop = gy + gh - ground_h;
    if let Some(g) = th.sprite_opt("window_ground") {
        let gf = g.frame(0);
        for yy in 0..ground_h {
            for xx in 0..gw {
                let p = gf.get_pixel((xx as u32) % gf.width(), (yy as u32) % gf.height()).0;
                put(img, gx + xx, gtop + yy, f(night_mul([p[0], p[1], p[2], 255])));
            }
        }
    }
    if let Some(far) = th.sprite_opt("window_view_far") {
        let ff = far.frame(0);
        clip(img, ff, gx + 1, gtop - ff.height() as i32 + 1);
    }
    let vf = view.frame(0);
    clip(img, vf, gx + gw - vf.width() as i32 - 1, gy + gh - vf.height() as i32);
    if gw >= 20 {
        let trim = f(th.colors.wall_trim);
        fill(img, gx + gw / 2 - 1, gy, 2, gh, trim);
    }
}

/// Council cabin geometry (relative to the pod origin).

/// The council cabin: a glass-walled meeting room (aluminium frames, a door gap in the front wall),
/// light carpet tiles, a wall screen with the agenda, a plant and a "COUNCIL" door sign. The
/// conference table and the seat items are drawn in the dynamic pass so members sit behind/in
/// front of it with the right depth. `th` = [floor, walls, furniture, decor] themes.
fn draw_council_cabin(img: &mut RgbaImage, th: [&Theme; 4], x: i32, y: i32) {
    let [floor_t, walls_t, furn_t, decor_t] = th;
    let f = |c: u32| furn_t.fx.apply(rgb(c));
    let fc = |c: Col| furn_t.fx.apply(c);
    let (w, h) = (POD_W, POD_H);
    // carpet tiles
    let mut cc = furn_t.colors.clone();
    cc.floor_a = floor_t.colors.floor_a;
    cc.rug = decor_t.colors.rug;
    let mut carpet = procedural::carpet(&cc, w - 6, h - 20);
    if !furn_t.fx.is_identity() {
        canvas::map_pixels(&mut carpet, &|c| furn_t.fx.apply(c));
    }
    blit(img, &carpet, x + 3, y + 14);
    // back wall with a skirting line
    let wall = fc(procedural::mix(walls_t.colors.wall, rgb(0xffffff), 0.5));
    fill(img, x + 1, y + 2, w - 2, 12, wall);
    fill(img, x + 1, y + 2, w - 2, 1, fc(shade(walls_t.colors.wall_trim, 1.1)));
    fill(img, x + 1, y + 12, w - 2, 2, fc(shade(walls_t.colors.wall_trim, 0.9)));
    fill(img, x + 1, y + 14, w - 2, 1, with_alpha(INK, 60));
    // wall screen showing the agenda
    let sx = x + w / 2 - 21;
    // (kept 3 px below the pod's top edge so the zone's nameplate strip above stays clear)
    fill(img, sx - 1, y + 3, 44, 10, f(0x0e1116));
    fill(img, sx, y + 4, 42, 8, f(0x16263d));
    fill(img, sx, y + 4, 42, 1, f(0x24406a));
    font::draw_text(img, sx + 3, y + 6, "AGENDA", f(0xdfe9f7));
    for (k, c) in [(0, 0x4cc46a), (1, 0xffd23f), (2, 0x82aaff)] {
        fill(img, sx + 29, y + 6 + k * 2, 2, 1, f(c));
        fill(img, sx + 32, y + 6 + k * 2, 8 - k * 2, 1, f(0x8fa3b8));
    }
    put(img, sx + 40, y + 10, f(0x5fd35f));
    // wall clock
    fill(img, x + w - 15, y + 4, 7, 7, f(0x30343a));
    fill(img, x + w - 14, y + 5, 5, 5, f(0xf4f1e6));
    put(img, x + w - 11, y + 6, f(0x20242a));
    put(img, x + w - 11, y + 7, f(0x20242a));
    put(img, x + w - 10, y + 8, f(0x20242a));
    // plant in the back-left corner
    let plant = procedural::plant(&decor_t.colors);
    let mut plant = plant;
    if !decor_t.fx.is_identity() {
        canvas::map_pixels(&mut plant, &|c| decor_t.fx.apply(c));
    }
    soft_shadow(img, furn_t, x + 4, y + 36, plant.width() as i32 - 1, 2);
    blit(img, &plant, x + 3, y + 15);
    // table shadow on the carpet (the table itself is drawn dynamically)
    let [tx, ty, tw, _] = layout::COUNCIL_TABLE;
    soft_shadow(img, furn_t, x + tx + 2, y + ty + 36, tw - 1, 5);
    // glass walls: pale glass between a bright and a dark aluminium rail
    let glass = with_alpha(f(0xbfe6ff), 62);
    let alu = f(0xd9dee4);
    let alu_d = f(0x7e8791);
    let door = (w / 2 - 12, 24);
    // side walls
    for (wx, lit) in [(x, true), (x + w - 3, false)] {
        fill(img, wx, y + 2, 3, h - 5, glass);
        fill(img, wx, y + 2, 1, h - 5, if lit { alu } else { alu_d });
        fill(img, wx + 2, y + 2, 1, h - 5, if lit { alu_d } else { alu });
        for k in (14..h - 8).step_by(24) {
            fill(img, wx, y + k, 3, 2, alu);
            fill(img, wx, y + k + 1, 3, 1, alu_d);
        }
        for k in (6..h - 10).step_by(11) {
            put(img, wx + 1, y + k, with_alpha(f(0xffffff), 190));
        }
    }
    // front wall, split by the door gap
    let fy = y + h - 5;
    for (fx0, fw) in [(x, door.0 + 1), (x + door.0 + door.1, w - door.0 - door.1)] {
        fill(img, fx0, fy, fw, 3, glass);
        fill(img, fx0, fy, fw, 1, alu);
        fill(img, fx0, fy + 2, fw, 1, alu_d);
        for k in (12..fw - 2).step_by(22) {
            fill(img, fx0 + k, fy, 2, 3, alu);
        }
    }
    // door posts, threshold and the leaf swung open into the room
    for px in [x + door.0, x + door.0 + door.1 - 2] {
        fill(img, px, fy - 1, 2, 5, alu);
        fill(img, px + 1, fy - 1, 1, 5, alu_d);
    }
    fill(img, x + door.0 + 2, fy + 1, door.1 - 4, 1, with_alpha(alu_d, 150));
    fill(img, x + door.0 + 2, fy - 15, 2, 14, with_alpha(f(0xbfe6ff), 110));
    fill(img, x + door.0 + 2, fy - 15, 1, 14, alu);
    put(img, x + door.0 + 3, fy - 8, f(0xe8c547));
    // door sign
    font::label(img, x + w / 2 + 1, fy - 11, "COUNCIL", f(0xf2f4f7), f(0x1d2733));
}

/// A council seat in the dynamic pass: the near-side chair back in front of its member, and the
/// laptop / papers / mug on the table in front of an occupied seat. `(ox, oy, d)` = cell + seat.
fn draw_council_seat(img: &mut RgbaImage, th: &Theme, chair_back: Option<&RgbaImage>, (ox, oy, d): (i32, i32, usize), who: Option<&Agent>, t: u64) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let near = d >= 2;
    if near {
        if let Some(cb) = chair_back {
            blit(img, cb, ox + 19, oy + 26);
        }
    }
    let Some(a) = who else { return };
    let working = !matches!(a.state, AgentState::Idle | AgentState::Done);
    let cx = ox + 26;
    let lid = f(0x59616b);
    if near {
        // laptop facing us (its screen is lit), keyboard deck below it, papers on the left
        let (lx, ly) = (cx - 5, oy - 8);
        fill(img, lx, ly, 11, 7, f(0x23272d));
        fill(img, lx + 1, ly + 1, 9, 5, f(0x101820));
        screen_fx(img, th, a.state, [lx + 1, ly + 1, 9, 5], t, character::hash(&a.id));
        fill(img, lx - 1, ly + 7, 13, 2, f(0xb9bec6));
        fill(img, lx - 1, ly + 7, 13, 1, f(0xdde1e6));
        fill(img, cx - 12, oy - 6, 6, 5, f(0xf4f1e6));
        fill(img, cx - 11, oy - 5, 4, 1, f(0x9a9688));
        fill(img, cx - 11, oy - 3, 3, 1, f(0x9a9688));
        let n = if working { 1 + ((t / 700) % 2) as i32 } else { 0 };
        for k in 0..n {
            fill(img, cx - 12 + k, oy - 7 - k, 6, 1, f(0xe0dccc));
        }
        // mug
        fill(img, cx + 8, oy - 4, 3, 3, f(0xf2f2f2));
        fill(img, cx + 8, oy - 4, 3, 1, f(0x5a3a22));
    } else {
        // laptop seen from behind (lid back) in front of the far-side member
        let (lx, ly) = (cx - 5, oy + 36);
        fill(img, lx, ly, 11, 6, lid);
        fill(img, lx, ly, 11, 1, shade(lid, 1.3));
        fill(img, lx, ly + 5, 11, 1, shade(lid, 0.7));
        put(img, lx + 5, ly + 2, f(0xdfe3e8));
        fill(img, lx - 1, ly + 6, 13, 1, f(0xb9bec6));
        fill(img, cx - 12, oy + 36, 6, 5, f(0xf4f1e6));
        fill(img, cx - 11, oy + 37, 4, 1, f(0x9a9688));
        fill(img, cx - 11, oy + 39, 3, 1, f(0x9a9688));
        fill(img, cx + 8, oy + 38, 3, 3, f(0xf2f2f2));
        fill(img, cx + 8, oy + 38, 3, 1, f(0x5a3a22));
    }
}

/// A seated council member on the near side of the table, seen from behind: the face is hidden
/// behind hair, the chair back (drawn after) covers the torso.
fn draw_char_back(img: &mut RgbaImage, th: &Theme, a: &Agent, pose: &Pose, x: i32, y: i32, selected: bool) {
    let mut buf = RgbaImage::new(character::CW as u32 + 4, character::CH as u32 + 8);
    let look = character::look_for(&a.id);
    character::draw(&mut buf, 2, 6, &look, pose);
    let hair = look.hair;
    let faces = [look.skin, shade(look.skin, 0.85), rgb(0x1a1a1a), rgb(0x30343a)];
    for yy in 6 + 2..6 + 9 {
        for xx in 2 + 3..2 + 14 {
            let p = buf.get_pixel(xx as u32, yy as u32).0;
            if p[3] > 0 && faces.iter().any(|f| f[..3] == p[..3]) {
                buf.put_pixel(xx as u32, yy as u32, image::Rgba(shade(hair, if yy < 6 + 5 { 1.0 } else { 0.9 })));
            }
        }
    }
    themed_sprite(&mut buf, th, selected);
    blit(img, &buf, x - 2, y - 6);
}

fn soft_shadow(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, w: i32, h: i32) {
    if th.fx.palette.is_empty() && w > 0 && h > 0 {
        fill(img, x - 1, y, w + 2, h, with_alpha(INK, 22));
        fill(img, x, y, w, h.max(2) - 1, with_alpha(INK, 38));
    }
}

/// Session floor colours (the lab is neutral teal-grey).
const ZONE_COLORS: [u32; 6] = [0x4aa3df, 0xb07cd8, 0xe0a23c, 0x52b788, 0xe06c8a, 0x7f8fe6];

fn zone_color(z: &layout::Zone) -> Col {
    match z.session {
        Some(_) => rgb(ZONE_COLORS[z.group % ZONE_COLORS.len()]),
        None => rgb(0x6f8f8c),
    }
}

/// CEO centre x within the corner office.
const CEO_CX: i32 = 32;

impl Renderer {
    /// Dynamic part of the CEO corner: the CEO, the desk in front, monitor, mug, nameplate.
    /// Returns the sprite top y and the mood.
    fn draw_ceo(&self, img: &mut RgbaImage, scene: &Scene, lay: &Layout, [cx, cy, cw, ch]: [i32; 4], t: u64, hl: bool) -> (i32, ceo::Mood) {
        let char_t = self.th(Layer::Characters);
        let furn = self.th(Layer::Furniture);
        let ccx = cx + CEO_CX;
        let alert_x = |id: &str| lay.seats.get(id).map(|s| s.x + CELL_W / 2);
        let mood = ceo::mood(scene, &self.ceo_look, ccx, alert_x, t);
        let top = if mood.standing() { cy + 5 } else { cy + 12 };
        let mut buf = RgbaImage::new(ceo::W as u32 + 16, ceo::H as u32 + 8);
        ceo::draw(&mut buf, 8, 4, &self.ceo_look, mood, (t + 333) % 4200 < 140);
        themed_sprite(&mut buf, char_t, hl);
        blit(img, &buf, ccx - ceo::W / 2 - 8, top - 4);

        // executive desk (in front of the CEO): dark wood, lit from the top-left, drawer pedestals
        // either side of a panelled kneehole, brass handles and a brass nameplate
        let ff = |c: Col| furn.fx.apply(c);
        let wood = shade(furn.colors.wood, 0.95);
        let ol = shade(wood, 0.4);
        let dt = cy + layout::CEO_DESK_TOP;
        let (dx, dw) = (cx + 2, cw - 4);
        soft_shadow(img, furn, dx + 2, dt + 16, dw - 1, 3);
        fill(img, dx, dt, dw, 16, ff(ol));
        fill(img, dx + 1, dt + 1, dw - 2, 6, ff(shade(wood, 1.2)));
        fill(img, dx + 1, dt + 1, dw - 2, 1, ff(shade(wood, 1.5)));
        fill(img, dx + 1, dt + 1, 1, 6, ff(shade(wood, 1.45)));
        fill(img, dx + 1, dt + 6, dw - 2, 1, ff(shade(wood, 0.95)));
        fill(img, dx + 1, dt + 7, dw - 2, 8, ff(shade(wood, 0.78)));
        fill(img, dx + 1, dt + 7, dw - 2, 1, ff(shade(wood, 0.6)));
        for px in [dx + 2, dx + dw - 20] {
            fill(img, px, dt + 8, 18, 7, ff(shade(wood, 0.66)));
            for k in 0..2 {
                let y = dt + 8 + k * 3;
                fill(img, px + 1, y, 16, 2, ff(shade(wood, 0.9)));
                fill(img, px + 1, y, 16, 1, ff(shade(wood, 1.05)));
                fill(img, px + 7, y, 4, 1, ff(rgb(0xe8c547)));
            }
        }
        fill(img, dx + 21, dt + 8, dw - 42, 7, ff(shade(wood, 0.55)));
        outline(img, dx + 21, dt + 8, dw - 42, 7, ff(shade(wood, 0.46)));
        fill(img, dx + 3, dt + 15, 3, 2, ff(furn.colors.metal));
        fill(img, dx + dw - 6, dt + 15, 3, 2, ff(furn.colors.metal));
        if let Some(rim) = furn.fx.palette.last() {
            // low-colour palettes: keep the desk readable against the floor
            outline(img, dx, dt, dw, 15, *rim);
        }
        // nameplate on the kneehole panel
        let name = font::truncate(&self.ceo.name, dw - 46);
        let plate_w = font::text_width(&name) + 4;
        let pcx = dx + dw / 2;
        fill(img, pcx - plate_w / 2 - 1, dt + 8, plate_w + 2, font::GLYPH_H + 2, ff(rgb(0x6b4a1e)));
        font::label(img, pcx, dt + 9, &name, ff(INK), ff(rgb(0xe8c547)));
        // desk-top clutter: lamp, papers, keyboard
        let lamp = self.th(Layer::Decor).sprite("lamp");
        blit(img, lamp.frame(0), cx + 54, dt + 4 - lamp.h());
        fill(img, cx + 38, dt + 3, 7, 3, ff(rgb(0xf4f1e6)));
        fill(img, cx + 38, dt + 5, 7, 1, ff(rgb(0xc9c4b4)));
        fill(img, cx + 39, dt + 3, 4, 1, ff(rgb(0x9a9688)));
        fill(img, cx + 8, dt + 4, 11, 2, ff(rgb(0x6b727c)));
        fill(img, cx + 8, dt + 4, 11, 1, ff(rgb(0xd3d7de)));
        // monitor with a little dashboard: one bar per agent state group
        let mon = furn.sprite("monitor_on");
        let (mx, my) = (cx + 8, dt + 4 - mon.h());
        blit(img, mon.frame(0), mx, my);
        let sr = mon.screen.unwrap_or([2, 2, mon.w() - 4, mon.h() - 6]);
        let [sx, sy, sw, sh] = [mx + sr[0], my + sr[1], sr[2], sr[3]];
        if sw > 2 && sh > 2 {
            fill(img, sx, sy, sw, sh, ff(rgb(0x14202e)));
            let n = scene.agents.len().max(1) as i32;
            let count = |f: &dyn Fn(AgentState) -> bool| scene.agents.values().filter(|a| f(a.state)).count() as i32;
            let groups: [(i32, u32); 3] = [
                (count(&|s| matches!(s, AgentState::Typing | AgentState::Reading | AgentState::Running | AgentState::Thinking | AgentState::Delegating)), 0x3fdc52),
                (count(&|s| matches!(s, AgentState::NeedsYou | AgentState::Error)), 0xff5050),
                (count(&|s| matches!(s, AgentState::Idle | AgentState::Done | AgentState::Queued)), 0x82aaff),
            ];
            let bw = ((sw - 1) / 3).max(1);
            for (i, (v, c)) in groups.iter().enumerate() {
                let bh = ((sh - 1) * v / n).max(if *v > 0 { 1 } else { 0 });
                fill(img, sx + 1 + i as i32 * bw, sy + sh - bh, (bw - 1).max(1), bh, ff(rgb(*c)));
            }
            if (t / 600) % 2 == 0 {
                put(img, sx + sw - 2, sy + 1, ff(rgb(0xb6ff9e)));
            }
            if let Some(g) = furn.glow {
                glow(img, [sx, sy, sw, sh], g, 4, 0.3);
            }
        }
        // mug on the desk unless it's in hand
        if !matches!(mood, ceo::Mood::Coffee { .. }) {
            let (ux, uy) = (cx + 48, dt + 2);
            fill(img, ux, uy, 3, 3, ff(rgb(0xf2f2f2)));
            put(img, ux + 3, uy + 1, ff(rgb(0xf2f2f2)));
            fill(img, ux, uy, 3, 1, ff(rgb(0x5a3a22)));
        }
        // the plant in the front-right corner stands in front of the desk
        let decor = self.th(Layer::Decor);
        let plant = decor.sprite("plant");
        soft_shadow(img, furn, cx + cw - 14, cy + ch - 2, plant.w() - 2, 2);
        blit(img, plant.frame(0), cx + cw - 13, cy + ch + 1 - plant.h());
        (top, mood)
    }
}

/// Apply a theme's colour fx to a sprite buffer, plus a rim on palette themes (or the highlight).
fn themed_sprite(buf: &mut RgbaImage, th: &Theme, selected: bool) {
    if !th.fx.is_identity() || th.fx.char_tint.is_some() {
        canvas::map_pixels(buf, &|c| th.fx.apply_char(c));
    }
    let rim = if selected { Some(th.fx.apply(SELECT)) } else { th.fx.palette.last().copied() };
    if let Some(sel) = rim {
        let src = buf.clone();
        let (w, h) = (src.width() as i32, src.height() as i32);
        let on = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h && src.get_pixel(x as u32, y as u32).0[3] > 0;
        for yy in 0..h {
            for xx in 0..w {
                if !on(xx, yy) && (on(xx - 1, yy) || on(xx + 1, yy) || on(xx, yy - 1) || on(xx, yy + 1)) {
                    put(buf, xx, yy, sel);
                }
            }
        }
    }
}

fn contact_shadow(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, w: i32, h: i32) {
    if th.fx.palette.is_empty() && w > 0 && h > 0 {
        fill(img, x, y, w, h, with_alpha(INK, 48));
    }
}

fn tool_glow_color(st: AgentState) -> Option<(Col, i32, f32)> {
    match st {
        AgentState::Typing => Some((rgb(0x5ea8ff), 7, 0.24)),
        AgentState::Running => Some((rgb(0x3fdc52), 7, 0.24)),
        AgentState::Reading => Some((rgb(0x4dd0e1), 6, 0.20)),
        AgentState::Thinking => Some((rgb(0xa78bfa), 6, 0.18)),
        AgentState::Delegating => Some((rgb(0xf59e0b), 6, 0.20)),
        AgentState::NeedsYou => Some((rgb(0xffd23f), 9, 0.32)),
        AgentState::Error => Some((rgb(0xff4444), 8, 0.28)),
        _ => None,
    }
}

fn draw_confetti(img: &mut RgbaImage, th: &Theme, cx: i32, head_y: i32, t: u64) {
    let phase = ((t / 150) % 4) as i32;
    for (dx, dy, c) in [(-5, -3 - phase, 0xffe14d), (5, -4 - (phase % 3), 0x4cc46a), (0, -6 - (phase % 2), 0x5ea8ff)] {
        put(img, cx + dx, head_y + dy, th.fx.apply(rgb(c)));
    }
}

/// Empty pixels required between two overlays (name tags, bubbles, badges).
const OVERLAY_GAP: i32 = 2;

/// Do two `[x, y, w, h]` rects collide, counting the required `OVERLAY_GAP` between them? Rects at
/// least `OVERLAY_GAP` px apart on either axis are clear (the old `+ 1` form let rects 1 px apart
/// pass as touching because the edge pixel counts too).
#[inline]
fn rects_overlap(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[0] + b[2] + OVERLAY_GAP
        && b[0] < a[0] + a[2] + OVERLAY_GAP
        && a[1] < b[1] + b[3] + OVERLAY_GAP
        && b[1] < a[1] + a[3] + OVERLAY_GAP
}

/// Does the text after a "Bash " prefix look like a shell command (rather than a description)?
fn looks_like_command(rest: &str) -> bool {
    const CMDS: [&str; 24] = [
        "cargo", "git", "rm", "ls", "cat", "npm", "npx", "pnpm", "yarn", "python", "python3", "sh", "bash", "cd", "grep", "rg", "make",
        "curl", "sed", "awk", "echo", "mkdir", "cp", "mv",
    ];
    let first = rest.split_whitespace().next().unwrap_or("");
    CMDS.contains(&first) || rest.contains(" -") || rest.contains("&&") || rest.contains('|') || rest.contains('$') || first.contains('/')
}

fn compact_activity(s: &str) -> String {
    let trimmed = s.trim();
    // a described Bash call reads better without the tool name: "run the tests", not "Bash run the tests"
    if let Some(rest) = trimmed.strip_prefix("Bash ").map(str::trim) {
        if !rest.is_empty() && !looks_like_command(rest) {
            return rest.to_string();
        }
    }
    if let Some((verb, rest)) = trimmed.split_once(' ') {
        if matches!(verb, "Edit" | "Read" | "Write" | "MultiEdit" | "View" | "write_file" | "read_file" | "edit_file")
            && rest.contains('/')
        {
            if let Some(base) = rest.rsplit('/').next().filter(|b| !b.is_empty()) {
                return format!("{verb} {base}");
            }
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone, Copy)]
enum Layer {
    Floor,
    Walls,
    Furniture,
    Decor,
    Characters,
}

fn tile(img: &mut RgbaImage, t: &RgbaImage, x0: i32, y0: i32, w: i32, h: i32) {
    let (tw, th) = (t.width() as i32, t.height() as i32);
    if tw == 0 || th == 0 {
        return;
    }
    // draw into a clipped sub-region so partial tiles don't spill
    let mut region = RgbaImage::new(w.max(0) as u32, h.max(0) as u32);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            blit(&mut region, t, x, y);
            x += tw;
        }
        y += th;
    }
    blit(img, &region, x0, y0);
}

fn glow(img: &mut RgbaImage, r: [i32; 4], c: Col, radius: i32, k: f32) {
    let [x, y, w, h] = r;
    for yy in y - radius..y + h + radius {
        for xx in x - radius..x + w + radius {
            let dx = (x - xx).max(xx - (x + w - 1)).max(0);
            let dy = (y - yy).max(yy - (y + h - 1)).max(0);
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            if d == 0.0 || d > radius as f32 {
                continue;
            }
            add(img, xx, yy, c, k * (1.0 - d / (radius as f32 + 1.0)));
        }
    }
}

/// Interpolate a walking position along a Manhattan polyline route.
fn walk_along(pts: &[(i32, i32)], start: u64, t: u64) -> Option<Place> {
    walk_along_min(pts, start, t, WALK_MIN_MS)
}

/// Like walk_along, but the walk takes at least `min_ms` (couriers between neighbouring desks
/// would otherwise blink past).
fn walk_along_min(pts: &[(i32, i32)], start: u64, t: u64, min_ms: f32) -> Option<Place> {
    let total: f32 = pts
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as f32)
        .sum();
    let dur = (total / WALK_PX_PER_S * 1000.0).max(min_ms);
    let p = (t.saturating_sub(start) as f32 / dur).clamp(0.0, 1.0);
    if p >= 1.0 {
        return None;
    }
    let mut rem = p * total;
    let last = pts.len().saturating_sub(2);
    for (i, w) in pts.windows(2).enumerate() {
        let len = ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as f32;
        if rem <= len || i == last {
            let q = if len > 0.0 { (rem / len).clamp(0.0, 1.0) } else { 1.0 };
            let fx = w[0].0 as f32 + (w[1].0 - w[0].0) as f32 * q;
            let fy = w[0].1 as f32 + (w[1].1 - w[0].1) as f32 * q;
            return Some(Place::Free {
                fx: fx.round() as i32,
                fy: fy.round() as i32,
                walk: Some(((t / 140) % 4) as u8),
            });
        }
        rem -= len;
    }
    None
}

/// Where an agent is drawn at time t.
fn place(a: &Agent, lay: &Layout, t: u64, queue_idx: usize) -> Place {
    place_with(a, lay, t, queue_idx, true)
}

/// `strolls` = false keeps idle agents at their desks (no coffee-bar breaks).
fn place_with(a: &Agent, lay: &Layout, t: u64, queue_idx: usize, strolls: bool) -> Place {
    let Some(seat) = lay.seats.get(&a.id) else { return Place::Hidden };
    let aisle = lay.seat_aisle(seat);
    let door = lay.door;
    match a.state {
        AgentState::Arriving => walk_along(&lay.walk_route(door, aisle), a.started_ms, t).unwrap_or(Place::Desk { standing: false }),
        AgentState::Leaving => walk_along(&lay.walk_route(aisle, door), a.last_event_ms, t).unwrap_or(Place::Hidden),
        AgentState::Queued => {
            let qx = (lay.lobby_bench.0 + queue_idx as i32 * 26).min(lay.w - 22);
            Place::Free { fx: qx, fy: lay.lobby_bench.1, walk: None }
        }
        AgentState::NeedsYou => Place::Desk { standing: true },
        AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS => Place::Desk { standing: true },
        AgentState::Idle if strolls && t.saturating_sub(a.last_event_ms) > 45_000 => {
            // Deterministic occasional coffee-bar breakroom stroll after 45s idle, anchored to last_event_ms + 45_000.
            let since = t.saturating_sub(a.last_event_ms) - 45_000;
            let slot = (since / 40_000).wrapping_add(character::hash(&a.id));
            if slot % 3 == 0 {
                let phase = since % 40_000;
                if phase < 12_000 {
                    let leg_t = t - phase;
                    let cb = (lay.coffee_bar.0.clamp(24, lay.w - 24), lay.coffee_bar.1);
                    if phase < 5_000 {
                        return walk_along(&lay.walk_route(aisle, cb), leg_t, t)
                            .unwrap_or(Place::Free { fx: cb.0, fy: cb.1, walk: None });
                    } else if phase < 8_000 {
                        return Place::Free { fx: cb.0, fy: cb.1, walk: None };
                    } else {
                        return walk_along(&lay.walk_route(cb, aisle), leg_t + 8_000, t)
                            .unwrap_or(Place::Desk { standing: false });
                    }
                }
            }
            Place::Desk { standing: false }
        }
        _ => Place::Desk { standing: false },
    }
}

fn pose_for(a: &Agent, t: u64, at_desk: bool) -> Pose {
    let h = character::hash(&a.id);
    let tt = t + (h % 997);
    let mut p = Pose { blink: tt % 4000 < 130, bob: if (tt / 700) % 2 == 0 { 0 } else { 1 }, ..Default::default() };
    match a.state {
        AgentState::Typing => {
            p.arms = Arms::Type((tt / 140) % 2 == 0);
            p.bob = 0;
        }
        AgentState::Running => p.arms = Arms::Type((tt / 450) % 2 == 0),
        AgentState::Reading => {
            p.arms = Arms::Page;
            p.head_dx = if (tt / 900) % 2 == 0 { 0 } else { 1 };
        }
        AgentState::Thinking => p.arms = Arms::Think,
        AgentState::Idle => {
            p.arms = Arms::Sip((tt / 3500) % 4 == 0);
            if (tt / 3200) % 5 == 0 {
                p.head_dx = if (tt / 1600) % 2 == 0 { -1 } else { 1 };
            }
        }
        AgentState::Delegating => p.arms = Arms::PointRight,
        AgentState::NeedsYou => {
            p.arms = Arms::Wave((t / 300) % 2 == 0);
            p.bob = 0;
        }
        AgentState::Error => {
            p.arms = Arms::Panic((t / 250) % 2 == 0);
            p.head_dx = if (t / 250) % 2 == 0 { 0 } else { -1 };
            p.bob = 0;
        }
        AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS => {
            p.arms = Arms::Up;
            p.bob = if (t / 150) % 2 == 0 { 0 } else { -1 };
        }
        _ => {}
    }
    if !at_desk && a.state == AgentState::Queued {
        p.bob = 0;
    }
    p
}

fn draw_char(img: &mut RgbaImage, th: &Theme, a: &Agent, pose: &Pose, x: i32, y: i32, selected: bool) {
    let mut buf = RgbaImage::new(character::CW as u32 + 4, character::CH as u32 + 8);
    character::draw(&mut buf, 2, 6, &character::look_for(&a.id), pose);
    // On palette-remapped themes (e.g. 2-colour retro) characters get a light rim so dark hair/pants
    // don't vanish into the floor; the selected agent gets the highlight rim instead.
    themed_sprite(&mut buf, th, selected);
    blit(img, &buf, x - 2, y - 6);
}

/// Monitor content per state.
fn screen_fx(img: &mut RgbaImage, th: &Theme, st: AgentState, r: [i32; 4], t: u64, seed: u64) {
    let [x, y, w, h] = r;
    if w <= 0 || h <= 0 {
        return;
    }
    let f = |c: u32| th.fx.apply(rgb(c));
    let hash = |k: u64| (k.wrapping_mul(0x9E3779B97F4A7C15) >> 33) as i32;
    match st {
        AgentState::Typing => {
            fill(img, x, y, w, h, f(0x1e2a3a));
            let step = (t / 220) as i32;
            for row in 0..(h / 2) {
                let len = 2 + hash(seed + row as u64 + (step / 3) as u64) % (w - 2).max(1);
                let len = if row == h / 2 - 1 { (step % (w - 1)).max(1) } else { len };
                let col = if row % 3 == 0 { 0xc792ea } else if row % 3 == 1 { 0x82aaff } else { 0xc3e88d };
                fill(img, x + 1 + (row % 2), y + row * 2, len.min(w - 1), 1, f(col));
            }
        }
        AgentState::Running => {
            fill(img, x, y, w, h, f(0x07120a));
            let scroll = (t / 110) as i32;
            for row in 0..h {
                let k = (row + scroll) as u64;
                if k % 2 == 0 {
                    let len = 2 + hash(seed ^ k) % (w - 2).max(1);
                    fill(img, x + 1, y + row, len.min(w - 1), 1, f(if k % 6 == 0 { 0xb6ff9e } else { 0x3fdc52 }));
                }
            }
        }
        AgentState::Reading => {
            fill(img, x, y, w, h, f(0xeef0f2));
            let scroll = (t / 300) as i32;
            for row in (1..h).step_by(2) {
                let k = (row + scroll) as u64;
                let len = 3 + hash(seed ^ k) % (w - 3).max(1);
                fill(img, x + 1, y + row, len.min(w - 2), 1, f(0x8a94a0));
            }
        }
        AgentState::Thinking => {
            fill(img, x, y, w, h, f(0x24304a));
            if (t / 400) % 2 == 0 {
                fill(img, x + 2, y + h - 3, 2, 2, f(0xe0e6f0));
            }
        }
        AgentState::NeedsYou => {
            fill(img, x, y, w, h, f(if (t / 350) % 2 == 0 { 0x5a4510 } else { 0x2a2410 }));
            fill(img, x + w / 2, y + 1, 1, h - 4, f(0xffd23f));
            put(img, x + w / 2, y + h - 2, f(0xffd23f));
        }
        AgentState::Error => {
            fill(img, x, y, w, h, f(0x4a1212));
            for k in 0..h.min(w) {
                put(img, x + w / 2 - h / 2 + k, y + k, f(0xff5050));
                put(img, x + w / 2 + h / 2 - k, y + k, f(0xff5050));
            }
        }
        AgentState::Delegating => {
            fill(img, x, y, w, h, f(0x1e2a3a));
            let active = ((t / 400) % 3) as i32;
            for k in 0..3 {
                let col = if k == active { 0xffe14d } else { 0x6b7a90 };
                outline(img, x + 1 + k * (w / 3), y + 2, (w / 3 - 1).max(2), (h - 4).max(2), f(col));
            }
        }
        AgentState::Idle => {
            fill(img, x, y, w, h, f(0x10141c));
            if (t / 900) % 2 == 0 {
                put(img, x + w - 2, y + h - 2, f(0x3a4a60));
            }
        }
        _ => {}
    }
}

fn icon_bubble(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, kind: AgentState, t: u64) {
    let f = |c: Col| th.fx.apply(c);
    let (bg, fg) = match kind {
        AgentState::NeedsYou => (f(rgb(0xffd23f)), f(INK)),
        AgentState::Error => (f(rgb(0xe03c3c)), f(PAPER)),
        AgentState::Done => (f(rgb(0x4cc46a)), f(PAPER)),
        _ => (f(PAPER), f(INK)),
    };
    fill(img, x, y, 9, 9, bg);
    outline(img, x, y, 9, 9, f(INK));
    put(img, x + 1, y + 9, f(INK));
    put(img, x + 2, y + 10, f(INK));
    match kind {
        AgentState::Thinking => {
            let n = (t / 300) % 4;
            for k in 0..3 {
                if (k as u64) < n {
                    put(img, x + 2 + k * 2, y + 5, fg);
                }
            }
        }
        AgentState::NeedsYou | AgentState::Error => {
            if kind == AgentState::Error || (t / 350) % 2 == 0 {
                fill(img, x + 4, y + 2, 1, 3, fg);
                put(img, x + 4, y + 6, fg);
            }
        }
        AgentState::Queued => {
            // clock
            outline(img, x + 2, y + 2, 5, 5, fg);
            put(img, x + 4, y + 3, fg);
            put(img, x + 4, y + 4, fg);
            let hand = [(5, 4), (4, 5), (3, 4), (4, 3)][((t / 400) % 4) as usize];
            put(img, x + hand.0, y + hand.1, fg);
        }
        AgentState::Done => {
            put(img, x + 2, y + 4, fg);
            put(img, x + 3, y + 5, fg);
            put(img, x + 4, y + 4, fg);
            put(img, x + 5, y + 3, fg);
            put(img, x + 6, y + 2, fg);
        }
        _ => {}
    }
}

fn with_alpha(c: Col, a: u8) -> Col {
    canvas::with_alpha(c, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer(mix: &str) -> Renderer {
        let dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes")];
        let mut cfg = Config::default();
        cfg.theme = ThemeMix { floor: mix.into(), walls: mix.into(), furniture: mix.into(), decor: mix.into(), characters: mix.into() };
        Renderer::with_dirs(&cfg, dirs)
    }

    #[test]
    fn show_toggles_change_pixels_and_keep_every_agent_seated() {
        use office_core::config::ShowConfig;
        let now = 1_000_000;
        let mut s = demo::demo_scene(now);
        for (i, a) in s.agents.values_mut().filter(|a| a.kind == AgentKind::ClaudeSub).take(2).enumerate() {
            a.role = Some(if i == 0 { "agy-council-architect".into() } else { "agy-council-advisor".into() });
        }
        let mut r = renderer("procedural");
        let base = r.compose(&s, None, 640, 360, now);
        let lay0 = layout::build(&s, 640, 360);
        assert!(lay0.pods.iter().any(|p| p.kind == layout::PodKind::Council), "council cabin exists by default");

        r.set_show(&ShowConfig { names: false, bubbles: false, ..ShowConfig::default() });
        let hidden = r.compose(&s, None, 640, 360, now);
        assert_eq!(hidden.dimensions(), base.dimensions());
        assert_ne!(hidden.as_raw(), base.as_raw(), "labels off changes the picture");

        // zones / council cabin off: static layer repaints and all agents still have a seat
        r.set_show(&ShowConfig::default());
        let again = r.compose(&s, None, 640, 360, now);
        assert_eq!(again.as_raw(), base.as_raw());
        r.set_show(&ShowConfig { zones: false, ..ShowConfig::default() });
        let nozones = r.compose(&s, None, 640, 360, now);
        assert_ne!(nozones.as_raw(), base.as_raw(), "zones off repaints the static layer");
        r.set_show(&ShowConfig { council_cabin: false, ..ShowConfig::default() });
        let _ = r.compose(&s, None, 640, 360, now);
        let lay = layout::build_with(&s, 640, 360, layout::Extras { ceo: true, council: false });
        assert!(lay.pods.iter().all(|p| p.kind != layout::PodKind::Council));
        assert_eq!(lay.seats.len(), lay0.seats.len());
        assert_eq!(lay.seats.len(), s.agents.len());
        // everything off must not panic either
        r.set_show(&ShowConfig {
            names: false, bubbles: false, icons: false, nameplates: false, zones: false, couriers: false,
            boss_visits: false, cat: false, ceo: false, council_cabin: false, ambient: false, selected_only_labels: true,
        });
        let _ = r.compose(&s, None, 640, 360, now);
        let _ = r.compose(&s, Some(&"cc:office".to_string()), 320, 200, now + 50_000);
    }

    #[test]
    fn compose_size_and_hit_test() {
        let mut r = renderer("procedural");
        let now = 10_000_000;
        let s = demo::demo_scene(now);
        let img = r.compose(&s, Some(&"cc:alpha".to_string()), 640, 360, now);
        assert_eq!(img.dimensions(), (640, 360));
        assert!(r.theme_errors().is_empty());
        let (x, y, w, h) = r.agent_rect(&"cc:alpha".to_string()).unwrap();
        assert_eq!(r.hit_test(x + w / 2, y + h / 2).as_deref(), Some("cc:alpha"));
        assert_eq!(r.hit_test(0, 0), None);
        let hit_of = |r: &Renderer, id: &str| {
            let (x, y, w, h) = r.agent_rect(&id.to_string()).expect("drawn");
            r.hit_test(x + w / 2, y + h / 2)
        };
        assert_eq!(hit_of(&r, CEO_ID).as_deref(), Some(CEO_ID));
        assert_eq!(hit_of(&r, CAT_ID).as_deref(), Some(CAT_ID));
        // big target → integer scale ≥ 2
        r.compose(&s, None, 1600, 900, now);
        assert!(r.scale() >= 2.0 && r.scale().fract() == 0.0);
        // tiny target → shrinks below 1 instead of cropping
        let img = r.compose(&s, None, 120, 80, now);
        assert_eq!(img.dimensions(), (120, 80));
        assert!(r.scale() < 1.0);
    }

    #[test]
    fn every_theme_and_empty_scene() {
        let now = 5_000_000;
        for t in ["procedural", "modern", "cozy", "midnight", "retro", "cyberpunk", "missing-theme"] {
            let mut r = renderer(t);
            let img = r.compose(&demo::demo_scene(now), None, 320, 200, now);
            assert_eq!(img.dimensions(), (320, 200));
            let img = r.compose(&Scene::default(), None, 320, 200, now);
            assert_eq!(img.dimensions(), (320, 200));
        }
        let r = renderer("missing-theme");
        assert!(!r.theme_errors().is_empty());
        assert!(r.themes().contains(&"retro".to_string()));
    }

    #[test]
    fn walkers_move_and_leavers_vanish() {
        let now = 1_000_000;
        let mut s = demo::demo_scene(now);
        let id = "cc-sub:beta/2".to_string();
        let lay = layout::build(&s, 640, 360);
        let a = &s.agents[&id];
        assert!(matches!(place(a, &lay, now, 0), Place::Free { walk: Some(_), .. }));
        assert!(matches!(place(a, &lay, now + 60_000, 0), Place::Desk { .. }));
        let l = s.agents.get_mut("agy:4104").unwrap();
        l.last_event_ms = now - 60_000;
        let l = &s.agents["agy:4104"];
        assert_eq!(place(l, &lay, now, 0), Place::Hidden);
    }

    /// Two seated, idle top-level Claude sessions (cat off so only trips cause walking).
    fn two_session_scene(now: u64) -> Scene {
        use office_core::{Event, EventKind, Reducer, SourceKind};
        let mut red = Reducer::new();
        for id in ["cc:a", "cc:b"] {
            red.apply(Event {
                at_ms: 0,
                source: SourceKind::Demo,
                kind: EventKind::AgentSeen {
                    id: id.into(),
                    kind: AgentKind::Claude,
                    label: Some(id.into()),
                    parent: None,
                    cwd: Some(format!("/tmp/{id}")),
                    role: None,
                    model: None,
                    slot: None,
                    pid: None,
                    resume_id: None,
                },
            });
        }
        let mut s = red.scene().clone();
        for a in s.agents.values_mut() {
            a.state = AgentState::Idle;
            a.started_ms = 0;
            a.last_event_ms = now;
        }
        s.now_ms = now;
        s
    }

    fn report(kind: office_core::ReportKind, from: &str, to: Option<&str>, at_ms: u64) -> office_core::Report {
        office_core::Report {
            id: 1,
            at_ms,
            kind,
            from: from.into(),
            from_label: from.into(),
            to: to.map(Into::into),
            to_label: to.unwrap_or("boss").into(),
            title: "t".into(),
            body: "b".into(),
            brief: String::new(),
        }
    }

    fn quiet_renderer() -> Renderer {
        let mut r = renderer("procedural");
        r.cat.enabled = false;
        r
    }

    #[test]
    fn result_report_sends_session_to_the_boss() {
        let t0 = 5_000_000;
        let mut s = two_session_scene(t0);
        let mut r = quiet_renderer();
        assert!(!r.compose_logical(&s, None, 640, 360, t0 + 500).is_walking);
        s.reports.push(report(office_core::ReportKind::Result, "cc:a", None, t0));
        assert!(r.compose_logical(&s, None, 640, 360, t0 + 500).is_walking);
        // long after the trip it is back at its desk
        assert!(!r.compose_logical(&s, None, 640, 360, t0 + TRIP_MAX_MS + 5_000).is_walking);
        // boss_trip itself reports a trip away from the desk
        let lay = layout::build(&s, 640, 360);
        let tr = r.boss_trip(&s.agents["cc:a"], &s, &lay, t0 + 500).expect("trip");
        assert!(!tr.approval && !tr.at_boss);
        assert!(r.boss_trip(&s.agents["cc:b"], &s, &lay, t0 + 500).is_none());
    }

    #[test]
    fn exit_plan_mode_session_ends_at_boss_spot() {
        let t0 = 7_000_000;
        let mut s = two_session_scene(t0);
        {
            let a = s.agents.get_mut("cc:a").unwrap();
            a.state = AgentState::NeedsYou;
            a.current_tool = Some("ExitPlanMode".into());
        }
        let mut r = quiet_renderer();
        let lay = layout::build(&s, 640, 360);
        let spot = boss_spot(&lay).expect("ceo office");
        assert!(r.compose_logical(&s, None, 640, 360, t0).is_walking);
        let tr = r.boss_trip(&s.agents["cc:a"], &s, &lay, t0 + 100).expect("trip");
        assert!(tr.approval && !tr.at_boss);
        r.compose(&s, None, 640, 360, t0 + 20_000);
        let tr = r.boss_trip(&s.agents["cc:a"], &s, &lay, t0 + 20_000).expect("trip");
        assert!(tr.approval && tr.at_boss);
        assert_eq!(tr.place, Place::Free { fx: spot.0, fy: spot.1, walk: None });
        assert!(!r.compose_logical(&s, None, 640, 360, t0 + 20_000).is_walking);
    }

    #[test]
    fn task_courier_between_seated_agents_is_walking() {
        let t0 = 9_000_000;
        let mut s = two_session_scene(t0);
        let mut r = quiet_renderer();
        assert!(!r.compose_logical(&s, None, 640, 360, t0 + 300).is_walking);
        s.reports.push(report(office_core::ReportKind::Task, "cc:a", Some("cc:b"), t0));
        assert!(r.compose_logical(&s, None, 640, 360, t0 + 300).is_walking);
    }

    #[test]
    fn animated_frames_vary_and_stroll_starts_at_phase_zero() {
        let s = demo::demo_scene(10_000);
        let mut r = Renderer::new(&Config::default());
        let f1 = r.compose(&s, None, 320, 200, 10_000);
        let f2 = r.compose(&s, None, 320, 200, 10_400);
        assert_ne!(f1.as_raw(), f2.as_raw());
    }

    /// A scene with a session, a worker, a council (architect + advisors) and a Chief Inspector.
    fn rosso_scene(now: u64) -> Scene {
        use office_core::{Event, EventKind, Reducer, SourceKind};
        let mut red = Reducer::new();
        let mut ev = |k: EventKind| {
            red.apply(Event { at_ms: now - 5_000, source: SourceKind::Demo, kind: k });
        };
        let seen = |id: &str, kind: AgentKind, label: &str, parent: Option<&str>, role: Option<&str>, cwd: Option<&str>| EventKind::AgentSeen {
            id: id.into(),
            kind,
            label: Some(label.into()),
            parent: parent.map(Into::into),
            cwd: cwd.map(Into::into),
            role: role.map(Into::into),
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        };
        ev(seen("cc:a", AgentKind::Claude, "cc:a", None, None, Some("/tmp/a")));
        ev(seen("agy:1", AgentKind::Agy, "Chief Inspector\u{b7}neon-medium", None, Some("supervisor"), Some("/tmp/a")));
        for (i, (role, label)) in [("agy-council-architect", "Chief Architect"), ("agy-council-advisor", "Security Seat"), ("agy-council-advisor", "Correctness Seat"), ("agy-council-advisor", "Reliability Seat")].iter().enumerate() {
            ev(seen(&format!("cc-sub:a/c{i}"), AgentKind::ClaudeSub, label, Some("cc:a"), Some(role), None));
        }
        let mail = |from: &str, to: &str, ty: &str, v: Option<&str>| EventKind::RossoMail {
            job: "j1".into(),
            from_title: from.into(),
            to_titles: vec![to.into()],
            doc_type: ty.into(),
            verdict: v.map(Into::into),
            subject: "x".into(),
            doc_path: None,
            escalation: false,
            body: String::new(),
        };
        ev(mail("Chief Architect", "Security Seat", "Task Card", None));
        ev(mail("Security Seat", "Chief Architect", "Critique", Some("OBJECT")));
        ev(mail("Correctness Seat", "Chief Architect", "Critique", Some("REVISE")));
        ev(EventKind::Verdict { id: "agy:1".into(), verdict: "APPROVE".into() });
        ev(EventKind::Failure {
            id: "agy:1".into(),
            info: office_core::FailureInfo { at_ms: now - 1_000, outcome: "ERROR/TIMEOUT".into(), diagnosis: "STALLED".into(), hint: "stalled: retry once".into() },
        });
        ev(EventKind::InTray { slug: "chief-inspector".into(), count: 3 });
        ev(EventKind::Entrypoint { id: "cc:a".into(), entrypoint: "claude-desktop".into() });
        ev(EventKind::RouterExtra(office_core::RouterExtra { orphans: 1, ..Default::default() }));
        ev(EventKind::GuardDecision { agent: "agy-tester".into(), tool: "Bash".into(), reason: "not allowed".into(), deny: true });
        let mut s = red.scene().clone();
        for a in s.agents.values_mut() {
            a.state = AgentState::Idle;
            a.started_ms = 0;
            a.last_event_ms = now;
        }
        s.guard.last_deny_ms = now - 500;
        s.now_ms = now;
        s
    }

    #[test]
    fn rosso_extras_render_and_change_the_picture() {
        let now = 9_000_000;
        let full = rosso_scene(now);
        assert_eq!(full.verdicts.len(), 3, "{:?}", full.verdicts);
        assert_eq!(full.in_trays.get("agy:1"), Some(&3));
        assert_eq!(full.entrypoints.len(), 1);
        let mut base = full.clone();
        base.guard = Default::default();
        base.verdicts.clear();
        base.failures.clear();
        base.in_trays.clear();
        base.entrypoints.clear();
        base.report_types.clear();
        base.router_extra = Default::default();
        let mut r = quiet_renderer();
        let b = r.compose(&base, None, 1280, 720, now);
        let f = r.compose(&full, None, 1280, 720, now);
        assert_ne!(b.as_raw(), f.as_raw());
        // every extra changes pixels on its own
        let mut each = [full.clone(), full.clone(), full.clone(), full.clone(), full.clone(), full.clone(), full.clone()];
        each[0].guard.last_deny_ms = 0; // guard hand down
        each[1].verdicts.clear();
        each[2].failures.clear();
        each[3].in_trays.clear();
        each[4].entrypoints.clear();
        each[5].router_extra = Default::default();
        each[6].report_types.clear(); // no council round
        for (i, e) in each.iter().enumerate() {
            let img = r.compose(e, None, 1280, 720, now);
            assert_ne!(img.as_raw(), f.as_raw(), "extra #{i} draws nothing");
        }
        if let Some(dir) = std::env::var_os("OFFICE_TEST_PNG_DIR") {
            let _ = f.save(std::path::Path::new(&dir).join("rosso-extras.png"));
        }
    }

    #[test]
    fn doc_icons_follow_the_document_type() {
        assert_eq!(doc_kind_for(Some("Critique"), false), DocKind::Critique);
        assert_eq!(doc_kind_for(Some("Rebuttal"), false), DocKind::Rebuttal);
        assert_eq!(doc_kind_for(Some("Decision Record"), false), DocKind::Decision);
        assert_eq!(doc_kind_for(Some("Sign-off Sheet"), false), DocKind::Signoff);
        assert_eq!(doc_kind_for(Some("Inspection Report"), false), DocKind::Inspection);
        assert_eq!(doc_kind_for(Some("Work Order"), true), DocKind::Task);
        assert_eq!(doc_kind_for(Some("Memo"), true), DocKind::Task);
        assert_eq!(doc_kind_for(None, false), DocKind::Report);
        // every icon draws something distinct
        let th = Renderer::new(&Config::default());
        let t = th.th(Layer::Characters);
        let mut seen = Vec::new();
        for k in [DocKind::Task, DocKind::Report, DocKind::Critique, DocKind::Rebuttal, DocKind::Decision, DocKind::Signoff, DocKind::Inspection] {
            let mut img = RgbaImage::new(12, 14);
            draw_doc(&mut img, t, 2, 3, k);
            assert!(!seen.contains(img.as_raw()), "{k:?} duplicates another icon");
            seen.push(img.as_raw().clone());
        }
    }

    #[test]
    fn verdict_chip_classes() {
        assert_eq!(extras::chip_text("CHANGES_REQUESTED"), "CHG");
        assert!(extras::chip_pulses("OBJECT") && extras::chip_pulses("REVISE") && extras::chip_pulses("FAIL"));
        assert!(!extras::chip_pulses("APPROVE"));
        assert_ne!(extras::chip_colors("PASS", 0).0, extras::chip_colors("FAIL", 0).0);
        assert_ne!(extras::chip_colors("FAIL", 0).0, extras::chip_colors("FAIL", 400).0, "red pulses");
        assert_eq!(extras::entry_tag("claude-desktop"), "DESKTOP");
        assert_eq!(extras::entry_tag("cli"), "CLI");
    }

    #[test]
    fn auto_strip_priority_over_wave_strip() {
        let now = 9_000_000;
        let mut r = quiet_renderer();
        let base = rosso_scene(now);

        let mut with_wave = base.clone();
        with_wave.waves.insert(
            "w1".into(),
            office_core::WaveState {
                job: "w1".into(),
                plan: "council".into(),
                updated_ms: now,
                done: 1,
                relay_inflight: 0,
                pending: 2,
                calls: vec![],
            },
        );
        let wave_img = r.compose(&with_wave, None, 1280, 720, now);

        // Running AutoJob overrides wave strip
        let mut with_running = with_wave.clone();
        with_running.auto_jobs.push(office_core::AutoJob {
            job: "a1".into(),
            flow: "council".into(),
            phase: "critique".into(),
            status: "running".into(),
            calls_used: 3,
            max_calls: 10,
            started_at: now,
            ..Default::default()
        });
        let running_img = r.compose(&with_running, None, 1280, 720, now);
        assert_ne!(running_img.as_raw(), wave_img.as_raw());

        // Non-running AutoJob defers to wave strip when a wave is present
        let mut with_done_and_wave = with_wave.clone();
        with_done_and_wave.auto_jobs.push(office_core::AutoJob {
            job: "a1".into(),
            flow: "council".into(),
            phase: "report".into(),
            status: "done".into(),
            calls_used: 8,
            max_calls: 10,
            started_at: now,
            ..Default::default()
        });
        let done_wave_img = r.compose(&with_done_and_wave, None, 1280, 720, now);
        assert_eq!(done_wave_img.as_raw(), wave_img.as_raw());

        // Non-running AutoJob renders when no wave is present
        let mut with_done_only = base;
        with_done_only.auto_jobs = with_done_and_wave.auto_jobs;
        let base_img = r.compose(&rosso_scene(now), None, 1280, 720, now);
        let done_only_img = r.compose(&with_done_only, None, 1280, 720, now);
        assert_ne!(done_only_img.as_raw(), base_img.as_raw());
    }
}
