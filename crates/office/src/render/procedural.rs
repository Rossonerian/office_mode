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
    let mut i = RgbaImage::new(40, 16);
    fill(&mut i, 0, 0, 40, 4, c.desk);
    fill(&mut i, 0, 3, 40, 1, shade(c.desk, 0.85));
    fill(&mut i, 1, 4, 38, 9, shade(c.desk, 0.72));
    // drawers
    outline(&mut i, 27, 5, 10, 4, shade(c.desk, 0.6));
    put(&mut i, 32, 6, c.metal);
    outline(&mut i, 27, 9, 10, 4, shade(c.desk, 0.6));
    put(&mut i, 32, 10, c.metal);
    // legs
    fill(&mut i, 2, 13, 2, 3, c.metal);
    fill(&mut i, 36, 13, 2, 3, c.metal);
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
    let mut i = RgbaImage::new(16, 16);
    fill(&mut i, 2, 1, 12, 10, c.chair);
    fill(&mut i, 3, 0, 10, 1, c.chair);
    fill(&mut i, 3, 2, 10, 1, shade(c.chair, 1.25));
    fill(&mut i, 1, 10, 14, 3, shade(c.chair, 0.8));
    fill(&mut i, 7, 13, 2, 2, c.metal);
    fill(&mut i, 3, 15, 10, 1, c.metal);
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
    let mut i = RgbaImage::new(9, 12);
    fill(&mut i, 2, 0, 6, 3, rgb(0xd9a441));
    fill(&mut i, 1, 3, 8, 1, shade(rgb(0xd9a441), 0.75));
    fill(&mut i, 4, 4, 1, 6, c.metal);
    fill(&mut i, 1, 10, 7, 2, c.metal);
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

/// All logical sprites for a colour set. (name -> (image, optional screen rect))
pub fn all(c: &Colors) -> HashMap<&'static str, (RgbaImage, Option<[i32; 4]>)> {
    let mut m = HashMap::new();
    m.insert("floor", (floor(c), None));
    m.insert("wall", (wall(c), None));
    m.insert("window", (window(c), None));
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
