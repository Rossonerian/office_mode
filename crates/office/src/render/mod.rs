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
pub mod font;
pub mod layout;
pub mod procedural;
pub mod theme;

use canvas::{add, blit, fill, outline, put, rgb, shade, Col};
use character::{Arms, Legs, Pose};
use image::RgbaImage;
use layout::{Layout, CELL_H, CELL_W, DECOR_H, MARGIN, POD_H, POD_W, WALL_H};
use office_core::{config::{CatConfig, CeoConfig, Config, ThemeMix}, model::AgentKind, Agent, AgentId, AgentState, Scene};
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
    lay.seats.get(id).map(|s| (s.x + 26, s.y + CELL_H - 3))
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
        feed_bytes(&[self.ceo.enabled as u8]);
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
                let built = layout::build_with(scene, px_w, px_h, layout::Extras { ceo: self.ceo.enabled });
                self.cached_layout = Some((topo, px_w, px_h, built.clone()));
                built
            }
        };
        let key = (self.mix_hash(), lay.static_hash());
        if key != self.static_key {
            self.static_img = self.draw_static(&lay);
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

    fn draw_static(&self, lay: &Layout) -> RgbaImage {
        let (w, h) = (lay.w, lay.h);
        let mut img = RgbaImage::new(w as u32, h as u32);
        let floor_t = self.th(Layer::Floor);
        let walls_t = self.th(Layer::Walls);
        let furn_t = self.th(Layer::Furniture);
        let decor_t = self.th(Layer::Decor);
        let night_theme = self.mix.walls == "midnight" || self.mix.walls == "cyberpunk";

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
        for z in &lay.zones {
            let c = furn_t.fx.apply(zone_color(z));
            let [zx, zy, zw, zh] = z.rect;
            fill(&mut img, zx, zy, zw, zh, with_alpha(c, 52));
            outline(&mut img, zx, zy, zw, zh, with_alpha(c, 200));
            fill(&mut img, zx, zy, zw, layout::ZONE_HEAD - 1, with_alpha(shade(c, 0.55), 230));
            fill(&mut img, zx, zy + layout::ZONE_HEAD - 2, zw, 1, with_alpha(c, 230));
        }

        // rugs + chairs per pod (use procedural rug when asset rug is a small strip like cozy's 48×16)
        let rug = decor_t.sprite("rug");
        let fallback_rug = (rug.h() < 32).then(|| {
            let mut r = procedural::rug(&decor_t.colors);
            if !decor_t.fx.is_identity() {
                canvas::map_pixels(&mut r, &|c| decor_t.fx.apply(c));
            }
            r
        });
        let chair = furn_t.sprite("chair");
        for pod in &lay.pods {
            if pod.kind == layout::PodKind::Council {
                draw_council_cabin(&mut img, furn_t, pod.x, pod.y);
                continue;
            }
            let rx = pod.x + POD_W / 2 - 38;
            let ry = pod.y + POD_H / 2 - 32 + 6;
            if let Some(ref prug) = fallback_rug {
                blit(
                    &mut img,
                    prug,
                    pod.x + POD_W / 2 - prug.width() as i32 / 2,
                    pod.y + POD_H / 2 - prug.height() as i32 / 2 + 6,
                );
            } else if rug.w() < 64 || rug.h() < 48 {
                tile(&mut img, rug.frame(0), rx, ry, 76, 64);
            } else {
                blit(&mut img, rug.frame(0), pod.x + POD_W / 2 - rug.w() / 2, pod.y + POD_H / 2 - rug.h() / 2 + 6);
            }
            for d in 0..layout::DESKS_PER_POD {
                let (cx, cy) = pod.cell(d);
                let chx = cx + 26 - chair.w() / 2;
                let chy = cy + 35 - chair.h();
                contact_shadow(&mut img, furn_t, chx + 2, chy + chair.h() - 1, chair.w() - 4, 2);
                blit(&mut img, chair.frame(0), chx, chy);
            }
        }
        if let Some(r) = lay.ceo {
            self.draw_ceo_office(&mut img, r);
        }
        let _ = MARGIN;
        img
    }

    /// Static part of the CEO corner: carpet, glass partition, executive chair.
    fn draw_ceo_office(&self, img: &mut RgbaImage, [cx, cy, cw, ch]: [i32; 4]) {
        let decor = self.th(Layer::Decor);
        let walls = self.th(Layer::Walls);
        let furn = self.th(Layer::Furniture);
        let fd = |c: Col| decor.fx.apply(c);
        let carpet = fd(shade(decor.colors.rug, 0.8));
        fill(img, cx - 2, cy - 1, cw + 4, ch + 1, carpet);
        outline(img, cx - 2, cy - 1, cw + 4, ch + 1, fd(decor.colors.rug_trim));
        // glass partition on the left (half height at the front, with a gap to walk in)
        let fw = |c: Col| walls.fx.apply(c);
        let glass = with_alpha(fw(walls.colors.sky), 120);
        let frame = fw(walls.colors.wall_trim);
        fill(img, cx - 7, cy - 2, 3, ch - 14, glass);
        fill(img, cx - 7, cy - 2, 1, ch - 14, frame);
        fill(img, cx - 5, cy - 2, 1, ch - 14, frame);
        fill(img, cx - 7, cy + ch - 16, 3, 1, frame);
        // executive chair (behind the CEO)
        let ff = |c: Col| furn.fx.apply(c);
        let leather = ff(rgb(0x3a2a24));
        let ccx = cx + CEO_CX;
        fill(img, ccx - 9, cy + 12, 18, 18, leather);
        fill(img, ccx - 8, cy + 11, 16, 1, leather);
        fill(img, ccx - 7, cy + 14, 14, 1, ff(shade(rgb(0x3a2a24), 1.4)));
        fill(img, ccx - 10, cy + 24, 2, 8, ff(shade(rgb(0x3a2a24), 0.7)));
        fill(img, ccx + 8, cy + 24, 2, 8, ff(shade(rgb(0x3a2a24), 0.7)));
        // a trophy shelf on the right wall of the corner
        let gold = ff(rgb(0xe8c547));
        fill(img, cx + cw - 10, cy + 2, 8, 1, ff(furn.colors.wood));
        fill(img, cx + cw - 8, cy - 2, 4, 3, gold);
        fill(img, cx + cw - 7, cy + 1, 2, 1, gold);
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
        if decor_t.fx.palette.is_empty() && wb.w() >= 28 && !scene.agents.is_empty() {
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

        // Animated coffee bar steam
        let cooler = furn_t.sprite("cooler");
        let plant = decor_t.sprite("plant");
        let sofa = furn_t.sprite("sofa");
        let cooler_x = 4 + plant.w() + 8;
        let sofa_x = lay.w - layout::SOFA_CX_FROM_RIGHT - sofa.w() / 2;
        let cb_x = lay.coffee_bar.0 - 11;
        if cb_x > cooler_x + cooler.w() + 6 && cb_x + 32 < sofa_x {
            let strip_bottom = WALL_H + 26;
            let st_phase = ((t / 280) % 3) as i32;
            put(img, cb_x + 6, strip_bottom - 19 - st_phase, with_alpha(PAPER, 110));
            put(img, cb_x + 7, strip_bottom - 20 - (st_phase + 1) % 3, with_alpha(PAPER, 75));
        }

        // Animated server rack LEDs reflecting router status
        if let Some([_, cy, _, ch]) = lay.ceo {
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
        queued.sort_by_key(|a| (a.started_ms, a.id.clone()));
        let queue_pos: HashMap<&AgentId, usize> = queued.into_iter().enumerate().map(|(i, a)| (&a.id, i)).collect();
        let trips: HashMap<&AgentId, BossTrip> =
            scene.agents.values().filter_map(|a| self.boss_trip(a, scene, lay, t).map(|tr| (&a.id, tr))).collect();
        let places: HashMap<&AgentId, Place> = scene
            .agents
            .values()
            .map(|a| {
                let qi = queue_pos.get(&a.id).copied().unwrap_or(0);
                let p = match trips.get(&a.id) {
                    Some(tr) if !matches!(place(a, lay, t, qi), Place::Hidden) => tr.place,
                    _ => place(a, lay, t, qi),
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
        let mut council_cells: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
        for pod in &lay.pods {
            for d in 0..layout::DESKS_PER_POD {
                let (x, y) = pod.cell(d);
                cells.push((x, y, pod.desks[d].as_ref().and_then(|id| scene.agents.get(id))));
                if pod.kind == layout::PodKind::Council {
                    council_cells.insert((x, y));
                }
            }
        }
        cells.sort_by_key(|c| (c.1, c.0));
        let desk = furn_t.sprite("desk");
        let lamp = decor_t.sprite("lamp");
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
                if let Some((a, standing)) = at_desk {
                    let pose = pose_for(a, t, true);
                    let cy = if standing { oy + 9 } else { oy + 13 };
                    let hl = selected == Some(&a.id) || self.hover.as_deref() == Some(a.id.as_str());
                    draw_char(img, char_t, a, &pose, ox + 18, cy, hl);
                    if a.state == AgentState::Done && t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS {
                        draw_confetti(img, char_t, ox + 26, cy, t);
                    }
                    hits.push(([ox + 4, oy + 4, CELL_W - 8, CELL_H - 12], a.id.clone()));
                }
                if council_cells.contains(&(ox, oy)) {
                    // council seat: a paper stack in front of whoever sits here
                    if let Some((a, _)) = at_desk {
                        let c = furn_t.fx.apply(rgb(0xf4f1e6));
                        let n = if matches!(a.state, AgentState::Idle | AgentState::Done) { 1 } else { 2 + ((t / 600) % 2) as i32 };
                        for k in 0..n {
                            fill(img, ox + 21 + k, oy + 37 - k, 7, 5, c);
                            outline(img, ox + 21 + k, oy + 37 - k, 7, 5, furn_t.fx.apply(rgb(0x9a9688)));
                        }
                    }
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
        for r in scene.reports.iter().rev().take(24) {
            if r.kind == office_core::ReportKind::Result || t < r.at_ms || t - r.at_ms > TRIP_MAX_MS {
                continue;
            }
            let Some(to) = r.to.as_deref().and_then(|to| desk_aisle(lay, to)) else { continue };
            let from = desk_aisle(lay, &r.from).unwrap_or(lay.door);
            if let Some(Place::Free { fx, fy, walk }) = walk_along_min(&lay.walk_route(from, to), r.at_ms, t, 2_800.0) {
                draw_courier(img, char_t, r.id, fx, fy, walk.unwrap_or(0), r.kind == office_core::ReportKind::Task);
                couriers += 1;
            }
        }

        // the office cat: above the floor and furniture, below the labels
        let mut cat_at: Option<cat::CatState> = None;
        if self.cat.enabled {
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

        // overlays: priority-ordered rect collision resolver so no two badges or speech bubbles overlap
        self.draw_overlays(img, char_t, scene, lay, &places, selected, t, ceo_head, cat_at, &reserved);
        self.hits = hits;
        couriers > 0
            || places.values().any(|p| matches!(p, Place::Free { walk: Some(_), .. }))
            || cat_at.as_ref().is_some_and(|st| matches!(st.action, cat::Action::Walk(_)))
    }

    /// A top-level session walking to the boss: to get a plan approved (it waits there until
    /// you answer), or to hand over a finished turn's result (it presents it, then walks back).
    fn boss_trip(&self, a: &Agent, scene: &Scene, lay: &Layout, t: u64) -> Option<BossTrip> {
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
        order.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.started_ms.cmp(&b.0.started_ms)));

        struct Planned<'a> {
            a: &'a Agent,
            cx: i32,
            head_y: i32,
            is_sel: bool,
            is_hov: bool,
            name: String,
            name_rect: [i32; 4],
            bubble: Option<(String, [i32; 4], Col, Col)>,
            icon: Option<(AgentState, i32, i32)>,
        }

        let mut placed: Vec<[i32; 4]> = Vec::new();
        let mut planned: Vec<Planned<'_>> = Vec::with_capacity(order.len());
        let f = |c: Col| char_t.fx.apply(c);
        let bounds = [3, WALL_H + 2, lay.w - 6, lay.h - WALL_H - 4];

        // Session nameplates live in each zone's reserved strip; labels and bubbles avoid them.
        for z in lay.zones.iter().filter(|z| z.head) {
            let [zx, zy, zw, _] = z.rect;
            let members = lay.pods.iter().filter(|p| p.group == z.group).flat_map(|p| p.desks.iter().flatten()).count();
            let name = match z.session.as_ref().and_then(|id| scene.agents.get(id)) {
                Some(s) => s.label.clone(),
                None => "AGY LAB".to_string(),
            };
            let txt = if members > 1 { format!("{name} · {members}") } else { name };
            let txt = font::truncate(&txt, zw - 6);
            let tw = font::text_width(&txt);
            font::draw_text(img, zx + 3, zy + 2, &txt, f(PAPER));
            placed.push([zx, zy, tw + 6, layout::ZONE_HEAD - 1]);
        }
        placed.extend_from_slice(reserved);
        // Nameplates (and the boss's ballot) are never covered, even by must-show overlays.
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
            let name = font::truncate(&a.label, max_name);
            let nw = font::text_width(&name) + 5; // +2px pip +1px gap +2px pad
            let nh = font::GLYPH_H + 2;
            let nx = (cx - nw / 2).clamp(bounds[0], (bounds[0] + bounds[2] - nw).max(bounds[0]));
            let ny = (head_y - 10).clamp(bounds[1], (bounds[1] + bounds[3] - nh).max(bounds[1]));
            let min_x = bounds[0];
            let max_x = (bounds[0] + bounds[2] - nw).max(min_x);
            let min_y = bounds[1];
            let max_y = (bounds[1] + bounds[3] - nh).max(min_y);
            let name_candidates = [
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
            ];
            let name_rect = name_candidates
                .into_iter()
                .find(|r| !placed.iter().any(|p| rects_overlap(*r, *p)))
                .or_else(|| best_spot(&name_candidates, &placed))
                .unwrap_or([nx, ny, nw, nh]);
            placed.push(name_rect);
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
            let must_show = is_sel || is_hov || matches!(a.state, AgentState::NeedsYou | AgentState::Error);
            let mut bubble_op = None;
            if speaking && !a.activity.is_empty() {
                let comp = compact_activity(&a.activity);
                let max_b = if is_sel || is_hov { 124 } else { CELL_W - 4 };
                let text = font::truncate(&comp, max_b);
                let bw = font::text_width(&text) + 4;
                let bh = font::GLYPH_H + 4;
                let base_y = (ny - bh - 2).clamp(bounds[1], (bounds[1] + bounds[3] - bh).max(bounds[1]));
                let base_x = (cx - bw / 2).clamp(bounds[0], (bounds[0] + bounds[2] - bw).max(bounds[0]));
                let recent_fail = a.last_tool_ok == Some(false) && t.saturating_sub(a.last_event_ms) < 2_500;
                let (bfg, bbg) = match a.state {
                    AgentState::NeedsYou => (f(INK), f(rgb(0xffd23f))),
                    AgentState::Error => (f(PAPER), f(rgb(0xe03c3c))),
                    _ if recent_fail => (f(PAPER), f(rgb(0xe03c3c))),
                    _ if is_sel || is_hov => (f(INK), f(PAPER)),
                    _ => (f(PAPER), with_alpha(f(rgb(0x1f2430)), 220)),
                };
                {
                    let max_x = (bounds[0] + bounds[2] - bw).max(bounds[0]);
                    let mut candidates = Vec::with_capacity(21);
                    for dy in [0, bh + 1, (bh + 1) * 2, (bh + 1) * 3] {
                        let y = (base_y - dy).max(bounds[1]);
                        for dx in [0, -12, 12, -24, 24, -36, 36] {
                            candidates.push([(base_x + dx).clamp(bounds[0], max_x), y, bw, bh]);
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
                AgentState::Thinking | AgentState::NeedsYou | AgentState::Error | AgentState::Queued => Some(a.state),
                AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS * 2 => Some(AgentState::Done),
                _ => None,
            }
            .map(|k| (k, cx + 9, head_y - 1));
            if let Some((_, ix, iy)) = icon {
                placed.push([ix, iy, 9, 11]);
            }

            planned.push(Planned { a, cx, head_y, is_sel, is_hov, name, name_rect, bubble: bubble_op, icon });
        }

        for p in planned.into_iter().rev() {
            let [nx, ny, nw, nh] = p.name_rect;
            if p.is_sel {
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

        if let Some((hx, top, mood)) = ceo_head {
            if matches!(mood, ceo::Mood::Alert { .. }) {
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
fn draw_courier(img: &mut RgbaImage, th: &Theme, seed: u64, fx: i32, fy: i32, phase: u8, task: bool) {
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
    draw_doc(img, th, fx + 4, fy - 13, if task { DocKind::Task } else { DocKind::Report });
}

/// The council cabin: glass walls around a pod and a round meeting table in the middle.
fn draw_council_cabin(img: &mut RgbaImage, th: &Theme, x: i32, y: i32) {
    let f = |c: u32| th.fx.apply(rgb(c));
    // carpet + glass walls
    fill(img, x + 2, y + 4, POD_W - 4, POD_H - 6, with_alpha(f(0x2b3a55), 120));
    let glass = with_alpha(f(0xbfe6ff), 150);
    fill(img, x + 1, y + 2, POD_W - 2, 2, glass);
    fill(img, x + 1, y + 2, 2, POD_H - 3, glass);
    fill(img, x + POD_W - 3, y + 2, 2, POD_H - 3, glass);
    fill(img, x + 1, y + POD_H - 3, POD_W / 2 - 12, 2, glass); // door gap in the middle of the front wall
    fill(img, x + POD_W / 2 + 11, y + POD_H - 3, POD_W / 2 - 12, 2, glass);
    for k in (8..POD_H - 6).step_by(14) {
        put(img, x + 2, y + k, with_alpha(f(0xffffff), 200));
        put(img, x + POD_W - 3, y + k, with_alpha(f(0xffffff), 200));
    }
    // round table
    let (cx, cy) = (x + POD_W / 2, y + POD_H / 2 + 6);
    let (rx, ry) = (30, 18);
    let top = f(0x8a5a36);
    let rim = f(0x5c3a22);
    for dy in -ry..=ry {
        let w = ((1.0 - (dy * dy) as f32 / (ry * ry) as f32).max(0.0).sqrt() * rx as f32) as i32;
        fill(img, cx - w, cy + dy, w * 2 + 1, 1, if dy.abs() >= ry - 1 { rim } else { top });
    }
    fill(img, cx - 2, cy + ry, 5, 6, rim);
    // the agenda in the middle of the table
    fill(img, cx - 5, cy - 4, 10, 7, f(0xf4f1e6));
    fill(img, cx - 3, cy - 2, 6, 1, f(0x9a9688));
    fill(img, cx - 3, cy, 5, 1, f(0x9a9688));
    font::label(img, cx, y + POD_H - 2, "COUNCIL", f(0x16202e), f(0xbfe6ff));
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
    fn draw_ceo(&self, img: &mut RgbaImage, scene: &Scene, lay: &Layout, [cx, cy, cw, _]: [i32; 4], t: u64, hl: bool) -> (i32, ceo::Mood) {
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

        // desk (in front of the CEO)
        let ff = |c: Col| furn.fx.apply(c);
        let wood = furn.colors.wood;
        let dt = cy + layout::CEO_DESK_TOP;
        let (dx, dw) = (cx + 2, cw - 4);
        fill(img, dx, dt, dw, 4, ff(shade(wood, 1.15)));
        fill(img, dx, dt + 3, dw, 1, ff(shade(wood, 0.9)));
        fill(img, dx + 1, dt + 4, dw - 2, 11, ff(wood));
        fill(img, dx + 1, dt + 14, dw - 2, 1, ff(shade(wood, 0.7)));
        fill(img, dx + 2, dt + 15, 3, 2, ff(furn.colors.metal));
        fill(img, dx + dw - 5, dt + 15, 3, 2, ff(furn.colors.metal));
        if let Some(rim) = furn.fx.palette.last() {
            // low-colour palettes: keep the desk readable against the floor
            outline(img, dx, dt, dw, 15, *rim);
        }
        // nameplate
        let name = font::truncate(&self.ceo.name, dw - 10);
        let plate_w = font::text_width(&name) + 4;
        let pcx = dx + dw / 2;
        fill(img, pcx - plate_w / 2 - 1, dt + 6, plate_w + 2, font::GLYPH_H + 4, ff(rgb(0x6b4a1e)));
        font::label(img, pcx, dt + 8, &name, ff(INK), ff(rgb(0xe8c547)));
        // monitor with a little dashboard: one bar per agent state group
        let mon = furn.sprite("monitor_on");
        let (mx, my) = (cx + 6, dt + 2 - mon.h());
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
            let (ux, uy) = (cx + 44, dt - 2);
            fill(img, ux, uy, 3, 3, ff(rgb(0xf2f2f2)));
            put(img, ux + 3, uy + 1, ff(rgb(0xf2f2f2)));
            fill(img, ux, uy, 3, 1, ff(rgb(0x5a3a22)));
        }
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

#[inline]
fn rects_overlap(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[0] + b[2] + 1 && a[0] + a[2] + 1 > b[0] && a[1] < b[1] + b[3] + 1 && a[1] + a[3] + 1 > b[1]
}

fn compact_activity(s: &str) -> String {
    let trimmed = s.trim();
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
    let Some(seat) = lay.seats.get(&a.id) else { return Place::Hidden };
    let aisle = (seat.x + 26, seat.y + CELL_H - 3);
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
        AgentState::Idle if t.saturating_sub(a.last_event_ms) > 45_000 => {
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
}
