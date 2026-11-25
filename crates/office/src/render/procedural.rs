//! The built-in `procedural` theme: every logical sprite drawn in code (original art).
//! Other themes use these as fallbacks, coloured by their `[colors]` table.
use super::canvas::{fill, outline, put, rgb, shade, Col};
use image::RgbaImage;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Colors {
    pub floor_a: Col,
    pub floor_b: Col,
    /// "checker" | "wood" | "plain"
    pub floor_style: String,
    pub wall: Col,
    pub wall_trim: Col,
    pub sky: Col,
    pub wood: Col,
    pub desk: Col,
    pub metal: Col,
    pub screen_off: Col,
    pub screen_on: Col,
    pub chair: Col,
    pub fabric: Col,
    pub plant: Col,
    pub pot: Col,
    pub rug: Col,
    pub rug_trim: Col,
    pub backdrop: Col,
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            floor_a: rgb(0xd9d4c7),
            floor_b: rgb(0xcbc5b6),
            floor_style: "checker".into(),
            wall: rgb(0x8fa3b8),
            wall_trim: rgb(0x5d6f82),
            sky: rgb(0x9fd6f2),
            wood: rgb(0x9c6b43),
            desk: rgb(0xe8e2d6),
            metal: rgb(0x4a4f58),
            screen_off: rgb(0x1d2228),
            screen_on: rgb(0x2c4a70),
            chair: rgb(0x3a4a6a),
            fabric: rgb(0x3f8f8a),
            plant: rgb(0x4f9a46),
            pot: rgb(0xb5653a),
            rug: rgb(0x7d5a8c),
            rug_trim: rgb(0xb99ac4),
            backdrop: rgb(0x1b1e24),
        }
    }
}

impl Colors {
    pub fn set(&mut self, key: &str, val: &str) {
        if key == "floor_style" {
            self.floor_style = val.to_string();
            return;
        }
        let Some(c) = super::canvas::parse_hex(val) else { return };
        let slot = match key {
            "floor_a" => &mut self.floor_a,
            "floor_b" => &mut self.floor_b,
            "wall" => &mut self.wall,
            "wall_trim" => &mut self.wall_trim,
            "sky" => &mut self.sky,
            "wood" => &mut self.wood,
            "desk" => &mut self.desk,
            "metal" => &mut self.metal,
            "screen_off" => &mut self.screen_off,
            "screen_on" => &mut self.screen_on,
            "chair" => &mut self.chair,
            "fabric" => &mut self.fabric,
            "plant" => &mut self.plant,
            "pot" => &mut self.pot,
            "rug" => &mut self.rug,
            "rug_trim" => &mut self.rug_trim,
            "backdrop" => &mut self.backdrop,
            _ => return,
        };
        *slot = c;
    }
}

/// Screen rectangle (x, y, w, h) inside the procedural monitor sprite.
pub const MONITOR_SCREEN: [i32; 4] = [2, 2, 12, 8];

pub fn floor(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(16, 16);
    match c.floor_style.as_str() {
        "wood" => {
            // 48×16: long boards 4 px tall with staggered seams, so it reads as planks, not bricks
            let mut i = RgbaImage::new(48, 16);
            fill(&mut i, 0, 0, 48, 16, c.floor_a);
            let seam_c = shade(c.floor_b, 1.05);
            for (row, seams) in [(0, [7, 31]), (1, [19, 43]), (2, [2, 27]), (3, [13, 38])] {
                let y = row * 4;
                fill(&mut i, 0, y + 3, 48, 1, c.floor_b);
                for sx in seams {
                    fill(&mut i, sx, y, 1, 3, seam_c);
                }
                fill(&mut i, (seams[0] + 9) % 48, y + 1, 4, 1, shade(c.floor_a, 1.06));
                if row % 2 == 0 {
                    fill(&mut i, 0, y, 48, 1, shade(c.floor_a, 1.03));
                }
            }
            return i;
        }
        "plain" => {
            fill(&mut i, 0, 0, 16, 16, c.floor_a);
            put(&mut i, 3, 4, c.floor_b);
            put(&mut i, 11, 12, c.floor_b);
        }
        _ => {
            fill(&mut i, 0, 0, 8, 8, c.floor_a);
            fill(&mut i, 8, 8, 8, 8, c.floor_a);
            fill(&mut i, 8, 0, 8, 8, c.floor_b);
            fill(&mut i, 0, 8, 8, 8, c.floor_b);
            put(&mut i, 0, 0, shade(c.floor_a, 0.94));
        }
    }
    i
}

pub fn wall(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(16, 16);
    fill(&mut i, 0, 0, 16, 16, c.wall);
    fill(&mut i, 0, 7, 16, 1, shade(c.wall, 0.93));
    fill(&mut i, 0, 15, 16, 1, shade(c.wall, 0.9));
    put(&mut i, 4, 3, shade(c.wall, 1.05));
    i
}

pub fn window(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(28, 18);
    fill(&mut i, 0, 0, 28, 18, c.wall_trim);
    fill(&mut i, 2, 2, 24, 13, c.sky);
    // light streaks + mullions
    for k in 0..4 {
        put(&mut i, 5 + k, 10 - k, shade(c.sky, 1.15));
        put(&mut i, 17 + k, 12 - k, shade(c.sky, 1.15));
    }
    fill(&mut i, 13, 2, 2, 13, c.wall_trim);
    fill(&mut i, 0, 15, 28, 3, shade(c.wall_trim, 1.2));
    i
}

pub fn door(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(18, 28);
    fill(&mut i, 0, 0, 18, 28, c.wall_trim);
    fill(&mut i, 2, 2, 14, 26, c.wood);
    outline(&mut i, 4, 4, 10, 9, shade(c.wood, 0.8));
    outline(&mut i, 4, 15, 10, 10, shade(c.wood, 0.8));
    put(&mut i, 13, 14, rgb(0xe8c547));
    i
}

pub fn desk(c: &Colors) -> RgbaImage {
    // 40×16 workstation desk lit from the top-left: bright top edge + left edge, dark outline,
    // drawer pedestal on the right and two steel feet.
    let mut i = RgbaImage::new(40, 16);
    let ol = shade(c.desk, 0.45);
    let hi = shade(c.desk, 1.1);
    let body = shade(c.desk, 0.7);
    fill(&mut i, 0, 0, 40, 14, ol);
    fill(&mut i, 1, 1, 38, 5, c.desk);
    fill(&mut i, 1, 1, 38, 1, hi);
    fill(&mut i, 1, 1, 1, 5, hi);
    fill(&mut i, 1, 5, 38, 1, shade(c.desk, 0.86));
    fill(&mut i, 1, 6, 38, 1, shade(c.desk, 0.78));
    fill(&mut i, 1, 7, 38, 6, body);
    fill(&mut i, 1, 7, 38, 1, shade(body, 0.85));
    // modesty panel seam + drawer pedestal
    fill(&mut i, 25, 7, 1, 6, ol);
    for k in 0..2 {
        let y = 8 + k * 3;
        fill(&mut i, 27, y, 10, 2, shade(c.desk, 0.78));
        fill(&mut i, 27, y, 10, 1, shade(c.desk, 0.9));
        fill(&mut i, 31, y, 2, 1, c.metal);
    }
    // cable tray (dark slot under the top at the back-left) and feet
    fill(&mut i, 4, 8, 16, 1, ol);
    fill(&mut i, 2, 13, 2, 3, c.metal);
    fill(&mut i, 36, 13, 2, 3, c.metal);
    fill(&mut i, 2, 13, 1, 3, shade(c.metal, 1.3));
    i
}

fn monitor(c: &Colors, screen: Col) -> RgbaImage {
    let mut i = RgbaImage::new(16, 14);
    fill(&mut i, 0, 0, 16, 12, c.metal);
    fill(&mut i, 2, 2, 12, 8, screen);
    put(&mut i, 13, 10, rgb(0x5fd35f));
    fill(&mut i, 7, 12, 2, 1, shade(c.metal, 0.8));
    fill(&mut i, 4, 13, 8, 1, shade(c.metal, 0.8));
    i
}

pub fn chair(c: &Colors) -> RgbaImage {
    // Task chair seen from the front with a seated person in front of it: tall mesh back with a
    // lit top-left edge, armrests and a gas stem on a five-star base (mostly hidden by the desk).
    let mut i = RgbaImage::new(16, 16);
    let ol = shade(c.chair, 0.45);
    fill(&mut i, 2, 0, 12, 11, ol);
    fill(&mut i, 3, 1, 10, 9, c.chair);
    fill(&mut i, 3, 1, 10, 1, shade(c.chair, 1.35));
    fill(&mut i, 3, 1, 1, 9, shade(c.chair, 1.2));
    fill(&mut i, 12, 2, 1, 8, shade(c.chair, 0.8));
    fill(&mut i, 5, 5, 6, 1, shade(c.chair, 0.85));
    // armrests
    fill(&mut i, 0, 8, 3, 2, ol);
    fill(&mut i, 13, 8, 3, 2, ol);
    fill(&mut i, 1, 8, 1, 1, shade(c.metal, 1.4));
    fill(&mut i, 14, 8, 1, 1, shade(c.metal, 1.4));
    fill(&mut i, 1, 10, 1, 2, c.metal);
    fill(&mut i, 14, 10, 1, 2, c.metal);
    // seat cushion
    fill(&mut i, 1, 10, 14, 4, ol);
    fill(&mut i, 2, 10, 12, 3, shade(c.chair, 1.12));
    fill(&mut i, 2, 10, 12, 1, shade(c.chair, 1.3));
    // stem and base
    fill(&mut i, 7, 14, 2, 1, c.metal);
    fill(&mut i, 2, 15, 12, 1, shade(c.metal, 0.8));
    fill(&mut i, 2, 15, 1, 1, c.metal);
    fill(&mut i, 13, 15, 1, 1, c.metal);
    i
}

/// A task chair seen from behind (14×14): used on the near side of the council table.
pub fn chair_back(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(14, 14);
    let ol = shade(c.chair, 0.45);
    fill(&mut i, 0, 0, 14, 10, ol);
    fill(&mut i, 1, 1, 12, 8, c.chair);
    fill(&mut i, 1, 1, 12, 1, shade(c.chair, 1.35));
    fill(&mut i, 1, 1, 1, 8, shade(c.chair, 1.2));
    fill(&mut i, 12, 2, 1, 7, shade(c.chair, 0.8));
    fill(&mut i, 3, 4, 8, 1, shade(c.chair, 0.86));
    fill(&mut i, 3, 6, 8, 1, shade(c.chair, 0.86));
    // seat edge, stem and caster base
    fill(&mut i, 1, 10, 12, 2, ol);
    fill(&mut i, 6, 12, 2, 1, c.metal);
    fill(&mut i, 2, 13, 10, 1, shade(c.metal, 0.8));
    put(&mut i, 2, 13, c.metal);
    put(&mut i, 11, 13, c.metal);
    i
}

pub fn plant(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(14, 22);
    let leaf = c.plant;
    let dark = shade(leaf, 0.75);
    let light = shade(leaf, 1.25);
    for (x, y, w, h) in [(5, 0, 4, 4), (2, 3, 4, 5), (8, 2, 4, 5), (0, 7, 5, 4), (9, 7, 5, 4), (4, 4, 6, 8)] {
        fill(&mut i, x, y, w, h, leaf);
    }
    for (x, y) in [(6, 1), (3, 4), (10, 3), (1, 8), (11, 8), (7, 6)] {
        put(&mut i, x, y, light);
    }
    for (x, y) in [(5, 10), (8, 9), (2, 10), (12, 10)] {
        put(&mut i, x, y, dark);
    }
    fill(&mut i, 3, 13, 8, 1, shade(c.pot, 1.15));
    fill(&mut i, 3, 14, 8, 8, c.pot);
    fill(&mut i, 4, 21, 6, 1, shade(c.pot, 0.7));
    i
}

pub fn sofa(c: &Colors) -> RgbaImage {
    let f = c.fabric;
    let mut i = RgbaImage::new(44, 20);
    fill(&mut i, 3, 0, 38, 10, shade(f, 0.85));
    fill(&mut i, 0, 6, 5, 12, f);
    fill(&mut i, 39, 6, 5, 12, f);
    fill(&mut i, 5, 10, 17, 7, shade(f, 1.12));
    fill(&mut i, 22, 10, 17, 7, shade(f, 1.12));
    fill(&mut i, 21, 10, 1, 7, shade(f, 0.8));
    fill(&mut i, 5, 17, 34, 1, shade(f, 0.7));
    fill(&mut i, 2, 18, 2, 2, c.metal);
    fill(&mut i, 40, 18, 2, 2, c.metal);
    i
}

pub fn cooler(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(12, 26);
    let water = rgb(0x6fc3ef);
    fill(&mut i, 3, 0, 6, 1, shade(water, 0.8));
    fill(&mut i, 2, 1, 8, 8, water);
    fill(&mut i, 3, 2, 1, 5, shade(water, 1.25));
    fill(&mut i, 4, 9, 4, 1, shade(water, 0.7));
    fill(&mut i, 1, 10, 10, 16, rgb(0xeef1f4));
    fill(&mut i, 10, 10, 1, 16, rgb(0xc8ccd2));
    put(&mut i, 3, 13, rgb(0xd84a4a));
    put(&mut i, 8, 13, rgb(0x4a7ad8));
    fill(&mut i, 3, 15, 6, 1, c.metal);
    i
}

pub fn whiteboard(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(36, 22);
    fill(&mut i, 0, 0, 36, 20, rgb(0xb8bec6));
    fill(&mut i, 1, 1, 34, 18, rgb(0xf7f8fa));
    // original scribbles: a little box diagram + lines
    outline(&mut i, 4, 4, 8, 5, rgb(0x2f6fd6));
    outline(&mut i, 20, 4, 8, 5, rgb(0x2f6fd6));
    fill(&mut i, 12, 6, 8, 1, rgb(0xd04a3a));
    put(&mut i, 19, 5, rgb(0xd04a3a));
    put(&mut i, 19, 7, rgb(0xd04a3a));
    fill(&mut i, 4, 12, 18, 1, rgb(0x404650));
    fill(&mut i, 4, 15, 12, 1, rgb(0x404650));
    fill(&mut i, 25, 12, 6, 4, rgb(0x2fa05a));
    fill(&mut i, 2, 20, 32, 2, c.metal);
    i
}

pub fn lamp(c: &Colors) -> RgbaImage {
    // articulated desk lamp: weighted base, thin arm, amber shade lit from the top-left
    let mut i = RgbaImage::new(9, 12);
    let amber = rgb(0xd9a441);
    fill(&mut i, 1, 0, 6, 1, shade(amber, 1.25));
    fill(&mut i, 0, 1, 8, 2, amber);
    fill(&mut i, 0, 3, 8, 1, shade(amber, 0.6));
    put(&mut i, 0, 1, shade(amber, 1.25));
    fill(&mut i, 4, 4, 1, 6, c.metal);
    put(&mut i, 5, 6, c.metal);
    fill(&mut i, 1, 10, 7, 2, shade(c.metal, 0.8));
    fill(&mut i, 1, 10, 7, 1, shade(c.metal, 1.3));
    i
}

pub fn rug(c: &Colors) -> RgbaImage {
    let (w, h) = (76, 64);
    let mut i = RgbaImage::new(w as u32, h as u32);
    fill(&mut i, 1, 0, w - 2, h, c.rug);
    fill(&mut i, 0, 1, w, h - 2, c.rug);
    outline(&mut i, 3, 3, w - 6, h - 6, c.rug_trim);
    for k in (8..w - 8).step_by(6) {
        put(&mut i, k, 6, c.rug_trim);
        put(&mut i, k, h - 7, c.rug_trim);
    }
    i
}

/// Espresso / coffee counter (22×18) for the back-wall break area.
pub fn coffee_bar(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(22, 18);
    // counter + shadow apron
    fill(&mut i, 0, 8, 22, 3, shade(c.wood, 1.15));
    fill(&mut i, 1, 11, 20, 6, shade(c.wood, 0.8));
    fill(&mut i, 1, 17, 2, 1, c.metal);
    fill(&mut i, 19, 17, 2, 1, c.metal);
    // espresso machine on the left
    fill(&mut i, 2, 1, 9, 8, c.metal);
    fill(&mut i, 3, 2, 7, 2, shade(c.metal, 1.35));
    fill(&mut i, 4, 5, 5, 3, rgb(0x181c22));
    put(&mut i, 8, 2, rgb(0x5fd35f)); // power led
    // ceramic mug under spout + spare cup on right
    fill(&mut i, 5, 6, 3, 2, rgb(0xf4f1ea));
    fill(&mut i, 15, 6, 3, 2, rgb(0xe8d5b5));
    fill(&mut i, 14, 7, 1, 1, rgb(0xe8d5b5));
    i
}

/// Server rack (14×26) with bay slots (animated LEDs drawn dynamically in render/mod.rs).
pub fn server_rack(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(14, 26);
    let body = shade(c.metal, 0.72);
    let trim = shade(c.metal, 1.25);
    fill(&mut i, 1, 0, 12, 25, body);
    outline(&mut i, 1, 0, 12, 25, trim);
    for bay in 0..4 {
        let y = 3 + bay * 5;
        fill(&mut i, 3, y, 8, 4, shade(body, 0.75));
        fill(&mut i, 4, y + 1, 4, 1, trim);
    }
    fill(&mut i, 2, 25, 2, 1, trim);
    fill(&mut i, 10, 25, 2, 1, trim);
    i
}

/// Lobby waiting bench (54×10) near the entrance door.
pub fn bench(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(54, 10);
    fill(&mut i, 1, 0, 52, 1, shade(c.fabric, 1.3));
    fill(&mut i, 1, 1, 52, 1, shade(c.fabric, 1.1));
    fill(&mut i, 1, 2, 52, 4, c.fabric);
    fill(&mut i, 0, 6, 54, 1, c.wood);
    fill(&mut i, 0, 7, 54, 1, shade(c.wood, 0.7));
    fill(&mut i, 3, 8, 2, 2, c.metal);
    fill(&mut i, 49, 8, 2, 2, c.metal);
    i
}

/// All logical sprites for a colour set. (name -> (image, optional screen rect))
pub fn all(c: &Colors) -> HashMap<&'static str, (RgbaImage, Option<[i32; 4]>)> {
    let mut m = HashMap::new();
    m.insert("floor", (floor(c), None));
    m.insert("wall", (wall(c), None));
    m.insert("window", (window(c), Some([2, 2, 24, 13])));
    m.insert("door", (door(c), None));
    m.insert("desk", (desk(c), None));
    m.insert("monitor_off", (monitor(c, c.screen_off), Some(MONITOR_SCREEN)));
    m.insert("monitor_on", (monitor(c, c.screen_on), Some(MONITOR_SCREEN)));
    m.insert("chair", (chair(c), None));
    m.insert("plant", (plant(c), None));
    m.insert("sofa", (sofa(c), None));
    m.insert("cooler", (cooler(c), None));
    m.insert("whiteboard", (whiteboard(c), None));
    m.insert("lamp", (lamp(c), None));
    m.insert("rug", (rug(c), None));
    m
}

pub fn mix(a: Col, b: Col, k: f32) -> Col {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round().clamp(0.0, 255.0) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), 255]
}

/// Light carpet tile colour for a colour set: a cool neutral at the floor's lightness, a hint of rug.
pub fn carpet_base(c: &Colors) -> Col {
    let lum = (0.299 * c.floor_a[0] as f32 + 0.587 * c.floor_a[1] as f32 + 0.114 * c.floor_a[2] as f32).clamp(40.0, 235.0);
    let l = (lum + 24.0).min(226.0);
    let cool = [(l * 0.93) as u8, (l * 0.97) as u8, (l * 1.04).min(255.0) as u8, 255];
    mix(cool, c.rug, 0.16)
}

/// Light office carpet tiles (8×8 tiles in two close tones with fine seams and a bound edge).
pub fn carpet(c: &Colors, w: i32, h: i32) -> RgbaImage {
    let mut i = RgbaImage::new(w as u32, h as u32);
    let base = carpet_base(c);
    let alt = shade(base, 0.965);
    for ty in 0..(h + 7) / 8 {
        for tx in 0..(w + 7) / 8 {
            let col = if (tx + ty) % 2 == 0 { base } else { alt };
            fill(&mut i, tx * 8, ty * 8, 8, 8, col);
            fill(&mut i, tx * 8, ty * 8, 8, 1, shade(col, 0.93));
            fill(&mut i, tx * 8, ty * 8, 1, 8, shade(col, 0.93));
        }
    }
    outline(&mut i, 0, 0, w, h, shade(base, 0.72));
    fill(&mut i, 1, 1, w - 2, 1, shade(base, 1.08));
    fill(&mut i, 1, 1, 1, h - 2, shade(base, 1.08));
    i
}

/// The joint between two desks of a bench (8×16): top, edge and modesty panel, no side outlines.
pub fn desk_bridge(c: &Colors) -> RgbaImage {
    let mut i = RgbaImage::new(8, 16);
    let ol = shade(c.desk, 0.45);
    fill(&mut i, 0, 0, 8, 14, ol);
    fill(&mut i, 0, 1, 8, 5, c.desk);
    fill(&mut i, 0, 1, 8, 1, shade(c.desk, 1.1));
    fill(&mut i, 0, 5, 8, 1, shade(c.desk, 0.86));
    fill(&mut i, 0, 6, 8, 1, shade(c.desk, 0.78));
    fill(&mut i, 0, 7, 8, 6, shade(c.desk, 0.7));
    // cable grommet in the top
    fill(&mut i, 3, 3, 2, 1, ol);
    i
}

/// Frosted privacy partition between two workstations (4×`h`), aluminium edged.
pub fn partition(c: &Colors, h: i32) -> RgbaImage {
    let mut i = RgbaImage::new(4, h as u32);
    let frame = shade(c.metal, 0.9);
    let frost = mix(c.sky, rgb(0xffffff), 0.6);
    fill(&mut i, 0, 0, 4, h, frame);
    fill(&mut i, 1, 1, 2, h - 3, frost);
    fill(&mut i, 1, 1, 1, h - 3, mix(frost, rgb(0xffffff), 0.6));
    fill(&mut i, 0, 0, 4, 1, shade(frame, 1.5));
    fill(&mut i, 0, h - 2, 4, 2, shade(c.metal, 0.7));
    i
}

/// Rectangular conference table (72×40): light laminate top lit from the top-left with a wood edge
/// band, a soft front shadow line and two steel panel legs.
pub fn council_table(c: &Colors) -> RgbaImage {
    let (w, h) = (72, 40);
    let mut i = RgbaImage::new(w as u32, h as u32);
    let top = mix(c.desk, rgb(0xffffff), 0.25);
    let edge = shade(c.wood, 1.05);
    let ol = shade(c.wood, 0.45);
    // legs first (under the front edge)
    for lx in [5, w - 9] {
        fill(&mut i, lx, 36, 4, 4, shade(c.metal, 0.75));
        fill(&mut i, lx, 36, 1, 4, shade(c.metal, 1.3));
    }
    fill(&mut i, 0, 0, w, 36, ol);
    fill(&mut i, 1, 1, w - 2, 31, top);
    fill(&mut i, 1, 1, w - 2, 1, shade(top, 1.06));
    fill(&mut i, 1, 1, 1, 31, shade(top, 1.06));
    fill(&mut i, w - 2, 2, 1, 30, shade(top, 0.9));
    fill(&mut i, 1, 31, w - 2, 1, shade(top, 0.86));
    // faint laminate sheen
    for k in 0..4 {
        fill(&mut i, 8 + k * 18, 6, 6, 1, shade(top, 1.04));
        fill(&mut i, 12 + k * 18, 20, 4, 1, shade(top, 1.04));
    }
    // centre-piece: a conference speakerphone and two water bottles
    fill(&mut i, w / 2 - 4, 14, 8, 4, shade(c.metal, 0.6));
    fill(&mut i, w / 2 - 3, 14, 6, 1, shade(c.metal, 1.4));
    put(&mut i, w / 2, 16, rgb(0x5fd35f));
    for bx in [w / 2 - 12, w / 2 + 9] {
        fill(&mut i, bx, 14, 2, 5, rgb(0x9ad4ee));
        fill(&mut i, bx, 14, 2, 1, rgb(0xe8eef2));
    }
    // front edge band
    fill(&mut i, 1, 32, w - 2, 3, edge);
    fill(&mut i, 1, 32, w - 2, 1, shade(edge, 1.2));
    fill(&mut i, 1, 34, w - 2, 1, shade(edge, 0.75));
    i
}
