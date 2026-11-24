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
use layout::{Layout, PodKind, CELL_H, CELL_W, DECOR_H, MARGIN, POD_H, POD_W, WALL_H};
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// At the desk (seated or standing behind it).
    Desk { standing: bool },
    /// Free-standing at feet position (fx, fy).
    Free { fx: i32, fy: i32, walk: Option<bool> },
    Hidden,
}

pub struct Renderer {
    mix: ThemeMix,
    dirs: Vec<PathBuf>,
    themes: HashMap<String, Theme>,
    errors: Vec<String>,
    static_key: String,
    static_img: RgbaImage,
    hits: Vec<([i32; 4], AgentId)>,
    view: View,
    logical: (u32, u32),
    cat: CatConfig,
    ceo: CeoConfig,
    ceo_look: ceo::CeoLook,
    hover: Option<AgentId>,
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
            static_key: String::new(),
            static_img: RgbaImage::new(1, 1),
            hits: Vec::new(),
            view: View { ox: 0.0, oy: 0.0, s: 1.0 },
            logical: (0, 0),
            cat: cfg.cat.clone(),
            ceo: cfg.ceo.clone(),
            ceo_look: ceo::look(&cfg.ceo),
            hover: None,
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
        self.static_key.clear();
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

    /// Compose the office for `scene` at time `t_ms` into a px_w × px_h image.
    /// `selected` gets a highlight ring.
    pub fn compose(&mut self, scene: &Scene, selected: Option<&AgentId>, px_w: u32, px_h: u32, t_ms: u64) -> image::RgbaImage {
        self.ensure_mix();
        let (px_w, px_h) = (px_w.max(1), px_h.max(1));
        let lay = layout::build_with(scene, px_w, px_h, layout::Extras { ceo: self.ceo.enabled });
        let key = format!("{:?}#{}", self.mix_names(), lay.static_key());
        if key != self.static_key {
            self.static_img = self.draw_static(&lay);
            self.static_key = key;
        }
        let mut frame = self.static_img.clone();
        self.hits.clear();
        self.draw_dynamic(&mut frame, scene, &lay, selected, t_ms);

        // scale into the output
        let (lw, lh) = (lay.w as f32, lay.h as f32);
        let fit = (px_w as f32 / lw).min(px_h as f32 / lh);
        let s = if fit >= 1.0 { fit.floor() } else { fit };
        let (ow, oh) = ((lw * s).floor(), (lh * s).floor());
        let ox = ((px_w as f32 - ow) / 2.0).floor().max(0.0);
        let oy = ((px_h as f32 - oh) / 2.0).floor().max(0.0);
        let bg = self.th(Layer::Walls).fx.apply(self.th(Layer::Walls).colors.backdrop);
        let raw = bg.repeat((px_w * px_h) as usize);
        let mut out = RgbaImage::from_raw(px_w, px_h, raw).expect("buffer size");
        canvas::scale_into(&mut out, &frame, ox as u32, oy as u32, s);
        self.view = View { ox, oy, s };
        self.logical = (lay.w as u32, lay.h as u32);
        out
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

    // ---------------------------------------------------------------- static layer

    fn draw_static(&self, lay: &Layout) -> RgbaImage {
        let (w, h) = (lay.w, lay.h);
        let mut img = RgbaImage::new(w as u32, h as u32);
        let floor_t = self.th(Layer::Floor);
        let walls_t = self.th(Layer::Walls);
        let furn_t = self.th(Layer::Furniture);
        let decor_t = self.th(Layer::Decor);

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
        let mut x = 14;
        while x + win.w() <= w - 12 {
            let clear_of_board = x + win.w() + 6 < wb_x || x > wb_x + wb.w() + 6;
            if clear_of_board {
                blit(&mut img, win.frame(0), x, (WALL_H - 3 - win.h()) / 2);
            }
            x += step;
        }
        blit(&mut img, wb.frame(0), wb_x, (WALL_H - wb.h()).max(2) / 2 + 2);

        // bottom wall strip with the door at the bottom-left
        let bh = 10;
        if let Some(bg) = walls_t.wall_bg {
            fill(&mut img, 0, h - bh, w, bh, bg);
        }
        tile(&mut img, &wt, 0, h - bh, w, bh);
        fill(&mut img, 0, h - bh, w, 2, trim);
        let door = walls_t.sprite("door");
        let door_x = lay.door.0 - door.w() / 2;
        blit(&mut img, door.frame(0), door_x, h - door.h());

        // decor strip along the back wall
        let strip_bottom = WALL_H + DECOR_H - 2;
        let plant = decor_t.sprite("plant");
        blit(&mut img, plant.frame(0), 4, strip_bottom - plant.h());
        blit(&mut img, plant.frame(0), w - 4 - plant.w(), strip_bottom - plant.h());
        let cooler = furn_t.sprite("cooler");
        blit(&mut img, cooler.frame(0), 4 + plant.w() + 8, strip_bottom - cooler.h());
        let sofa = furn_t.sprite("sofa");
        blit(&mut img, sofa.frame(0), w - layout::SOFA_CX_FROM_RIGHT - sofa.w() / 2, strip_bottom - sofa.h());
        // a plant by the door, too
        blit(&mut img, plant.frame(0), w - 4 - plant.w(), h - bh - plant.h() + 2);

        // rugs + chairs per pod
        let rug = decor_t.sprite("rug");
        let chair = furn_t.sprite("chair");
        for pod in &lay.pods {
            blit(&mut img, rug.frame(0), pod.x + POD_W / 2 - rug.w() / 2, pod.y + POD_H / 2 - rug.h() / 2 + 6);
            for d in 0..layout::DESKS_PER_POD {
                let (cx, cy) = pod.cell(d);
                blit(&mut img, chair.frame(0), cx + 26 - chair.w() / 2, cy + 35 - chair.h());
            }
            if pod.kind == PodKind::Lab {
                let c = self.th(Layer::Characters).fx.apply(KIND_AGY);
                font::label(&mut img, pod.x + 2 + font::text_width("AGY LAB") / 2, pod.y + POD_H - 7, "AGY LAB", c, with_alpha(INK, 200));
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

    fn draw_dynamic(&mut self, img: &mut RgbaImage, scene: &Scene, lay: &Layout, selected: Option<&AgentId>, t: u64) {
        let furn_t = self.th(Layer::Furniture);
        let decor_t = self.th(Layer::Decor);
        let char_t = self.th(Layer::Characters);
        let mut hits: Vec<([i32; 4], AgentId)> = Vec::new();

        // queue order for Queued agents
        let mut queued: Vec<&Agent> = scene.agents.values().filter(|a| a.state == AgentState::Queued).collect();
        queued.sort_by_key(|a| (a.started_ms, a.id.clone()));
        let places: HashMap<&AgentId, Place> = scene
            .agents
            .values()
            .map(|a| {
                let qi = queued.iter().position(|q| q.id == a.id).unwrap_or(0);
                (&a.id, place(a, lay, t, qi))
            })
            .collect();

        // desks row by row (so lower rows overlap upper ones)
        let mut cells: Vec<(i32, i32, Option<&Agent>)> = Vec::new();
        for pod in &lay.pods {
            for d in 0..layout::DESKS_PER_POD {
                let (x, y) = pod.cell(d);
                cells.push((x, y, pod.desks[d].as_ref().and_then(|id| scene.agents.get(id))));
            }
        }
        cells.sort_by_key(|c| (c.1, c.0));
        let desk = furn_t.sprite("desk");
        let lamp = decor_t.sprite("lamp");
        for (ox, oy, ag) in &cells {
            let (ox, oy) = (*ox, *oy);
            let at_desk = ag.and_then(|a| match places.get(&a.id) {
                Some(Place::Desk { standing }) => Some((a, *standing)),
                _ => None,
            });
            if let Some((a, standing)) = at_desk {
                let pose = pose_for(a, t, true);
                let cy = if standing { oy + 9 } else { oy + 13 };
                draw_char(img, char_t, a, &pose, ox + 18, cy, selected == Some(&a.id));
                hits.push(([ox + 4, oy + 4, CELL_W - 8, CELL_H - 12], a.id.clone()));
            }
            let desk_top = oy + 48 - desk.h();
            blit(img, desk.frame(0), ox + 24 - desk.w() / 2, desk_top);
            // monitor
            let on_state = at_desk.map(|(a, _)| a.state);
            let mon = furn_t.sprite(if on_state.is_some() { "monitor_on" } else { "monitor_off" });
            let (mx, my) = (ox + 13 - mon.w() / 2, desk_top + 4 - mon.h());
            blit(img, mon.frame((t / 500) as usize), mx, my);
            if let Some(st) = on_state {
                let sr = mon.screen.unwrap_or([2, 2, mon.w() - 4, mon.h() - 6]);
                let scr = [mx + sr[0], my + sr[1], sr[2], sr[3]];
                screen_fx(img, furn_t, st, scr, t, at_desk.map(|(a, _)| character::hash(&a.id)).unwrap_or(0));
                if let Some(g) = furn_t.glow {
                    glow(img, scr, g, 5, 0.35);
                }
            }
            // lamp (blinks when the agent needs you)
            let (lx, ly) = (ox + 40 - lamp.w() / 2, desk_top + 4 - lamp.h());
            blit(img, lamp.frame(0), lx, ly);
            if on_state == Some(AgentState::NeedsYou) && (t / 350) % 2 == 0 {
                let c = decor_t.fx.apply(rgb(0xffd23f));
                glow(img, [lx + lamp.w() / 2 - 2, ly, 4, 3], c, 6, 0.55);
                fill(img, lx + lamp.w() / 2 - 2, ly + lamp.h() / 3, 4, 2, c);
            }
        }

        // free-standing agents (walking, queued)
        let mut free: Vec<(&Agent, i32, i32, Option<bool>)> = scene
            .agents
            .values()
            .filter_map(|a| match places.get(&a.id) {
                Some(Place::Free { fx, fy, walk }) => Some((a, *fx, *fy, *walk)),
                _ => None,
            })
            .collect();
        free.sort_by_key(|f| (f.2, f.1));
        for (a, fx, fy, walk) in &free {
            let mut pose = pose_for(a, t, false);
            if let Some(ph) = walk {
                pose.legs = Legs::Walk(*ph);
                pose.arms = Arms::Swing(*ph);
                pose.bob = 0;
            }
            draw_char(img, char_t, a, &pose, fx - 8, fy - 24, selected == Some(&a.id));
            hits.push(([fx - 9, fy - 26, 18, 27], a.id.clone()));
        }

        // the CEO in the corner office
        let mut ceo_head: Option<(i32, i32, ceo::Mood)> = None;
        if let Some(r) = lay.ceo {
            let hl = selected == Some(&CEO_ID.to_string()) || self.hover.as_deref() == Some(CEO_ID);
            let (top, mood) = self.draw_ceo(img, scene, lay, r, t, hl);
            hits.push(([r[0] + CEO_CX - 10, top - 2, 20, r[1] + layout::CEO_DESK_TOP - top + 2], CEO_ID.to_string()));
            ceo_head = Some((r[0] + CEO_CX, top, mood));
        }
        // the office cat: above the floor and furniture, below the labels
        let mut cat_at: Option<cat::CatState> = None;
        if self.cat.enabled {
            let st = cat::state(lay, scene, t);
            let hl = selected == Some(&CAT_ID.to_string()) || self.hover.as_deref() == Some(CAT_ID);
            let mut buf = RgbaImage::new(cat::BOX_W as u32 + 8, cat::BOX_H as u32 + 4);
            cat::draw(&mut buf, 4 + cat::BOX_W / 2, 2 + cat::BOX_H, &st, &cat::coat(&self.cat.color));
            themed_sprite(&mut buf, char_t, hl);
            blit(img, &buf, st.x - cat::BOX_W / 2 - 4, st.y - cat::BOX_H - 2);
            hits.push(([st.x - cat::BOX_W / 2 - 1, st.y - cat::BOX_H - 1, cat::BOX_W + 2, cat::BOX_H + 2], CAT_ID.to_string()));
            cat_at = Some(st);
        }

        // overlays: labels, bubbles, icons (selected last so it sits on top)
        let mut order: Vec<&Agent> = scene.agents.values().filter(|a| !matches!(places.get(&a.id), Some(Place::Hidden) | None)).collect();
        order.sort_by_key(|a| (selected == Some(&a.id), a.started_ms));
        for a in order {
            let is_sel = selected == Some(&a.id);
            let (cx, head_y) = match places[&a.id] {
                Place::Desk { standing } => {
                    let s = lay.seats[&a.id];
                    (s.x + 24, if standing { s.y + 10 } else { s.y + 14 })
                }
                Place::Free { fx, fy, .. } => (fx, fy - 24),
                Place::Hidden => continue,
            };
            overlays(img, char_t, a, cx, head_y, t, is_sel);
        }
        let f = |c: Col| char_t.fx.apply(c);
        if let Some((hx, top, mood)) = ceo_head {
            if matches!(mood, ceo::Mood::Alert { .. }) {
                icon_bubble(img, char_t, hx + 6, top - 9, AgentState::NeedsYou, t);
            }
        }
        if let Some(st) = cat_at {
            // visible top of the sprite (the sleeping loaf is lower)
            let top = st.y - if st.action == cat::Action::Sleep { 6 } else { cat::BOX_H };
            let named = selected == Some(&CAT_ID.to_string()) || self.hover.as_deref() == Some(CAT_ID);
            let mut name_w = 0;
            if named && !self.cat.name.is_empty() {
                let name = font::truncate(&self.cat.name, 80);
                name_w = font::text_width(&name);
                font::label(img, st.x, top - 8, &name, f(INK), f(SELECT));
            }
            if st.action == cat::Action::Sleep {
                let txt = ["z", "zZ", "zZz"][((t / 700) % 3) as usize];
                let dy = ((t / 350) % 2) as i32;
                let x0 = st.x + 3 + name_w / 2;
                font::label(img, x0 + font::text_width(txt) / 2, top - 7 - dy, txt, f(PAPER), with_alpha(f(INK), 150));
            }
        }
        self.hits = hits;
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
    if !th.fx.is_identity() {
        canvas::map_pixels(buf, &|c| th.fx.apply(c));
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

/// Where an agent is drawn at time t.
fn place(a: &Agent, lay: &Layout, t: u64, queue_idx: usize) -> Place {
    let Some(seat) = lay.seats.get(&a.id) else { return Place::Hidden };
    let aisle = (seat.x + 26, seat.y + CELL_H - 3);
    let door = lay.door;
    let walk = |from: (i32, i32), to: (i32, i32), start: u64| -> Option<Place> {
        let dist = (((to.0 - from.0).pow(2) + (to.1 - from.1).pow(2)) as f32).sqrt();
        let dur = (dist / WALK_PX_PER_S * 1000.0).max(WALK_MIN_MS);
        let p = (t.saturating_sub(start) as f32 / dur).clamp(0.0, 1.0);
        if p >= 1.0 {
            return None;
        }
        let fx = from.0 as f32 + (to.0 - from.0) as f32 * p;
        let fy = from.1 as f32 + (to.1 - from.1) as f32 * p;
        Some(Place::Free { fx: fx.round() as i32, fy: fy.round() as i32, walk: Some((t / 180) % 2 == 0) })
    };
    match a.state {
        AgentState::Arriving => walk(door, aisle, a.started_ms).unwrap_or(Place::Desk { standing: false }),
        AgentState::Leaving => walk(aisle, door, a.last_event_ms).unwrap_or(Place::Hidden),
        AgentState::Queued => Place::Free { fx: door.0 + 22 + queue_idx as i32 * 14, fy: door.1, walk: None },
        AgentState::NeedsYou => Place::Desk { standing: true },
        AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS => Place::Desk { standing: true },
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
        AgentState::Delegating => p.arms = Arms::PointRight,
        AgentState::NeedsYou => {
            p.arms = Arms::RaiseRight;
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
            for row in (1..h).step_by(2) {
                let len = 3 + hash(seed + row as u64) % (w - 3).max(1);
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
            for k in 0..3 {
                outline(img, x + 1 + k * (w / 3), y + 2, (w / 3 - 1).max(2), (h - 4).max(2), f(0xffcb6b));
            }
        }
        _ => {}
    }
}

fn bubble(img: &mut RgbaImage, th: &Theme, cx: i32, bottom: i32, text: &str, fg: Col, bg: Col) -> i32 {
    let tw = font::text_width(text);
    let (w, h) = (tw + 4, font::GLYPH_H + 4);
    let x = cx - w / 2;
    let y = bottom - h - 2;
    let border = th.fx.apply(INK);
    fill(img, x, y, w, h, bg);
    outline(img, x, y, w, h, border);
    // tail
    put(img, cx, y + h, border);
    put(img, cx - 1, y + h, bg);
    put(img, cx, y + h + 1, border);
    font::draw_text(img, x + 2, y + 2, text, fg);
    y
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

fn overlays(img: &mut RgbaImage, th: &Theme, a: &Agent, cx: i32, head_y: i32, t: u64, selected: bool) {
    let f = |c: Col| th.fx.apply(c);
    // name label above the head
    let max = if selected { 120 } else { CELL_W - 4 };
    let name = font::truncate(&a.label, max);
    let label_y = head_y - 9;
    let bg = if selected { f(SELECT) } else { f(shade(INK, 1.0)) };
    let fg = if selected { f(INK) } else { f(kind_color(a.kind)) };
    font::label(img, cx, label_y, &name, fg, with_alpha(bg, if selected { 255 } else { 210 }));
    // activity speech bubble
    let speaking = matches!(
        a.state,
        AgentState::Typing | AgentState::Reading | AgentState::Running | AgentState::Delegating | AgentState::NeedsYou | AgentState::Error | AgentState::Queued
    );
    if speaking && !a.activity.is_empty() {
        let text = font::truncate(&a.activity, if selected { 140 } else { CELL_W - 6 });
        bubble(img, th, cx, label_y - 1, &text, f(INK), f(PAPER));
    }
    // state icon by the head
    let icon = match a.state {
        AgentState::Thinking | AgentState::NeedsYou | AgentState::Error | AgentState::Queued => Some(a.state),
        AgentState::Done if t.saturating_sub(a.last_event_ms) < DONE_CHEER_MS * 2 => Some(AgentState::Done),
        _ => None,
    };
    if let Some(k) = icon {
        icon_bubble(img, th, cx + 9, head_y - 1, k, t);
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
        for t in ["procedural", "modern", "cozy", "retro", "cyberpunk", "missing-theme"] {
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
}
