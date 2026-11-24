//! Original procedural pixel characters (16×24 at 1×), varied deterministically per agent id.
use super::canvas::{fill, put, rgb, shade, Col};
use image::RgbaImage;

pub const CW: i32 = 16;
pub const CH: i32 = 24;

#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub skin: Col,
    pub hair: Col,
    pub hair_style: u8,
    pub shirt: Col,
    pub pants: Col,
    pub shoes: Col,
    pub glasses: bool,
}

pub fn hash(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const SKINS: [u32; 6] = [0xf6d3b3, 0xe8b48f, 0xc98e62, 0xa56a43, 0x7a4a2c, 0xf1c7a0];
const HAIRS: [u32; 7] = [0x2b1d14, 0x5a3a22, 0x8c5a2b, 0xd8b45a, 0xb33a2a, 0x1c1c22, 0x9aa0a8];
const SHIRTS: [u32; 9] = [0x3f7fd0, 0xd0583f, 0x3fae6a, 0x8f5fc8, 0xe0a83a, 0x2f9fa8, 0xd04f8c, 0x55606e, 0xf0f0f0];
const PANTS: [u32; 4] = [0x2d3a55, 0x3b3b3b, 0x6b5640, 0x4a5a3a];

pub fn look_for(id: &str) -> Look {
    let h = hash(id);
    let pick = |shift: u32, n: usize| ((h >> shift) as usize) % n;
    Look {
        skin: rgb(SKINS[pick(0, SKINS.len())]),
        hair: rgb(HAIRS[pick(8, HAIRS.len())]),
        hair_style: pick(16, 5) as u8,
        shirt: rgb(SHIRTS[pick(24, SHIRTS.len())]),
        pants: rgb(PANTS[pick(32, PANTS.len())]),
        shoes: rgb(0x26221f),
        glasses: pick(40, 4) == 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arms {
    Down,
    /// Hands on the desk; bool alternates which hand is up.
    Type(bool),
    Up,
    RaiseRight,
    PointRight,
    Page,
    /// Swinging while walking; bool = phase.
    Swing(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legs {
    Stand,
    Walk(bool),
}

#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub arms: Arms,
    pub legs: Legs,
    /// Head shift in x (tilt while reading).
    pub head_dx: i32,
    /// Upper body bob in y (breathing).
    pub bob: i32,
    pub blink: bool,
}

impl Default for Pose {
    fn default() -> Self {
        Pose { arms: Arms::Down, legs: Legs::Stand, head_dx: 0, bob: 0, blink: false }
    }
}

/// Draw a character into `img` with its top-left at (ox, oy).
pub fn draw(img: &mut RgbaImage, ox: i32, oy: i32, l: &Look, p: &Pose) {
    let px = |img: &mut RgbaImage, x: i32, y: i32, c: Col| put(img, ox + x, oy + y, c);
    let rect = |img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Col| fill(img, ox + x, oy + y, w, h, c);
    let shirt_d = shade(l.shirt, 0.78);
    let skin_d = shade(l.skin, 0.85);
    let b = p.bob;

    // legs + shoes
    match p.legs {
        Legs::Stand => {
            rect(img, 5, 17, 2, 6, l.pants);
            rect(img, 9, 17, 2, 6, l.pants);
            rect(img, 4, 23, 3, 1, l.shoes);
            rect(img, 9, 23, 3, 1, l.shoes);
        }
        Legs::Walk(ph) => {
            let (a, c) = if ph { (0, 1) } else { (1, 0) };
            rect(img, 5, 17, 2, 6 - a, l.pants);
            rect(img, 9, 17, 2, 6 - c, l.pants);
            rect(img, 4, 23 - a, 3, 1, l.shoes);
            rect(img, 9, 23 - c, 3, 1, l.shoes);
        }
    }
    // torso
    rect(img, 4, 10 + b, 8, 7, l.shirt);
    rect(img, 4, 16 + b, 8, 1, shirt_d);
    rect(img, 7, 10 + b, 2, 1, skin_d); // collar
    rect(img, 5, 17, 6, 1, shade(l.pants, 0.8)); // belt
    // arms
    let sleeve = |img: &mut RgbaImage, x: i32, y: i32, h: i32| {
        rect(img, x, y, 1, h, shirt_d);
    };
    match p.arms {
        Arms::Down => {
            sleeve(img, 3, 11 + b, 5);
            sleeve(img, 12, 11 + b, 5);
            px(img, 3, 16 + b, l.skin);
            px(img, 12, 16 + b, l.skin);
        }
        Arms::Swing(ph) => {
            let (a, c) = if ph { (-1, 1) } else { (1, -1) };
            sleeve(img, 3, 11 + b, 5);
            sleeve(img, 12, 11 + b, 5);
            px(img, 3, 16 + b + a.max(0), l.skin);
            px(img, 12, 16 + b + c.max(0), l.skin);
        }
        Arms::Type(alt) => {
            sleeve(img, 3, 11 + b, 4);
            sleeve(img, 12, 11 + b, 4);
            let (ly, ry) = if alt { (15, 16) } else { (16, 15) };
            rect(img, 4, ly + b, 2, 1, shirt_d);
            rect(img, 10, ry + b, 2, 1, shirt_d);
            px(img, 6, ly + b, l.skin);
            px(img, 9, ry + b, l.skin);
        }
        Arms::Up => {
            rect(img, 2, 4 + b, 1, 7, shirt_d);
            rect(img, 13, 4 + b, 1, 7, shirt_d);
            rect(img, 2, 3 + b, 1, 1, l.skin);
            rect(img, 13, 3 + b, 1, 1, l.skin);
            px(img, 3, 10 + b, shirt_d);
            px(img, 12, 10 + b, shirt_d);
        }
        Arms::RaiseRight => {
            sleeve(img, 3, 11 + b, 5);
            px(img, 3, 16 + b, l.skin);
            rect(img, 13, 3 + b, 1, 8, shirt_d);
            rect(img, 13, 1 + b, 1, 2, l.skin);
            px(img, 12, 10 + b, shirt_d);
        }
        Arms::PointRight => {
            sleeve(img, 3, 11 + b, 5);
            px(img, 3, 16 + b, l.skin);
            rect(img, 12, 11 + b, 3, 1, shirt_d);
            rect(img, 15, 11 + b, 1, 1, l.skin);
        }
        Arms::Page => {
            sleeve(img, 3, 11 + b, 3);
            sleeve(img, 12, 11 + b, 3);
            rect(img, 5, 11 + b, 6, 5, rgb(0xf4f1e6));
            rect(img, 6, 12 + b, 4, 1, rgb(0x9aa3ad));
            rect(img, 6, 14 + b, 3, 1, rgb(0x9aa3ad));
            px(img, 4, 14 + b, l.skin);
            px(img, 11, 14 + b, l.skin);
        }
    }
    // head
    let hx = 4 + p.head_dx;
    let hy = 2 + b;
    rect(img, hx, hy, 8, 8, l.skin);
    rect(img, 7, 9 + b, 2, 1, skin_d); // chin/neck
    if p.blink {
        rect(img, hx + 2, hy + 4, 1, 1, skin_d);
        rect(img, hx + 5, hy + 4, 1, 1, skin_d);
    } else {
        px(img, hx + 2, hy + 4, rgb(0x1a1a1a));
        px(img, hx + 5, hy + 4, rgb(0x1a1a1a));
    }
    px(img, hx + 3, hy + 6, skin_d);
    px(img, hx + 4, hy + 6, skin_d);
    if l.glasses {
        rect(img, hx + 1, hy + 3, 6, 1, rgb(0x30343a));
    }
    // hair
    let hc = l.hair;
    let hd = shade(hc, 0.8);
    match l.hair_style {
        0 => {
            // short crop
            rect(img, hx, hy - 1, 8, 3, hc);
            rect(img, hx, hy + 2, 1, 2, hc);
            rect(img, hx + 7, hy + 2, 1, 2, hc);
            rect(img, hx + 1, hy - 1, 6, 1, hd);
        }
        1 => {
            // long
            rect(img, hx - 1, hy - 1, 10, 3, hc);
            rect(img, hx - 1, hy + 2, 2, 8, hc);
            rect(img, hx + 7, hy + 2, 2, 8, hc);
            rect(img, hx + 2, hy - 1, 4, 1, hd);
        }
        2 => {
            // bun
            rect(img, hx, hy - 1, 8, 3, hc);
            rect(img, hx + 2, hy - 4, 4, 3, hc);
            rect(img, hx + 3, hy - 4, 2, 1, hd);
            rect(img, hx, hy + 2, 1, 1, hc);
            rect(img, hx + 7, hy + 2, 1, 1, hc);
        }
        3 => {
            // spiky
            rect(img, hx, hy, 8, 2, hc);
            for k in 0..4 {
                rect(img, hx + k * 2, hy - 2 + (k % 2), 1, 2, hc);
                px(img, hx + k * 2 + 1, hy - 1, hd);
            }
        }
        _ => {
            // cap
            rect(img, hx, hy - 1, 8, 3, l.shirt);
            rect(img, hx + 1, hy - 2, 6, 1, l.shirt);
            rect(img, hx + 6, hy + 1, 4, 1, shade(l.shirt, 0.7));
            rect(img, hx, hy + 2, 1, 2, hc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_varied() {
        let a = look_for("cc:abc");
        let b = look_for("cc:abc");
        assert_eq!(a.shirt, b.shirt);
        assert_eq!(a.hair_style, b.hair_style);
        let distinct: std::collections::HashSet<_> =
            (0..40).map(|i| { let l = look_for(&format!("agy:{i}")); (l.shirt, l.hair, l.hair_style) }).collect();
        assert!(distinct.len() > 20);
    }

    #[test]
    fn draws_within_box() {
        let mut img = RgbaImage::new(CW as u32 + 4, CH as u32 + 8);
        for arms in [Arms::Down, Arms::Up, Arms::RaiseRight, Arms::PointRight, Arms::Page, Arms::Type(true)] {
            draw(&mut img, 2, 4, &look_for("x"), &Pose { arms, ..Default::default() });
        }
        assert!(img.pixels().any(|p| p.0[3] > 0));
    }
}
