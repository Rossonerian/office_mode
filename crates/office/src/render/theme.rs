//! Theme packs: `assets/themes/<name>/theme.toml` maps logical sprite names to sheet regions.
//!
//! ```toml
//! [sheets]
//! city = { file = "roguelikeCity.png", tile = 16, spacing = 1, key = "#ff00ff" }
//! [sprites]
//! floor = { sheet = "city", col = 7, row = 20 }                   # one tile
//! door  = { sheet = "city", tiles = [[26,26],[26,27]], grid = [1,2] } # stitched tiles
//! desk  = { sheet = "city", x = 0, y = 0, w = 32, h = 16, frames = 1, tint = "#ffffff" }
//! monitor_on = { sheet = "city", col = 1, row = 2, screen = [2,2,12,8] }
//! [colors]   # colours for procedural fallbacks (see procedural::Colors)
//! floor_style = "wood"
//! [fx]
//! tint = "#rrggbb"            # multiply every sprite
//! palette = ["#000", "#fff"]   # luminance remap (after tint)
//! glow = "#00ffff"             # glowing monitors
//! floor_bg = "#101010"         # fill under floor tiles (for transparent tilesets)
//! wall_bg = "#101010"
//! ```
//! Any missing logical sprite is filled from the procedural set (with this theme's colours and fx).
use super::canvas::{crop, map_pixels, parse_hex, Col};
use super::procedural::{self, Colors};
use anyhow::{anyhow, Context, Result};
use image::RgbaImage;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const SPRITE_NAMES: [&str; 14] = [
    "floor", "wall", "window", "door", "desk", "monitor_off", "monitor_on", "chair", "plant", "sofa", "cooler",
    "whiteboard", "lamp", "rug",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    sheets: HashMap<String, SheetDef>,
    sprites: HashMap<String, SpriteDef>,
    colors: HashMap<String, String>,
    fx: FxDef,
}

#[derive(Debug, Deserialize)]
struct SheetDef {
    file: String,
    #[serde(default = "d16")]
    tile: u32,
    #[serde(default)]
    spacing: u32,
    #[serde(default)]
    key: Option<String>,
}
fn d16() -> u32 {
    16
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SpriteDef {
    sheet: String,
    x: Option<u32>,
    y: Option<u32>,
    w: Option<u32>,
    h: Option<u32>,
    col: Option<u32>,
    row: Option<u32>,
    cols: Option<u32>,
    rows: Option<u32>,
    tiles: Option<Vec<[u32; 2]>>,
    grid: Option<[u32; 2]>,
    frames: Option<u32>,
    tint: Option<String>,
    screen: Option<[i32; 4]>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FxDef {
    tint: Option<String>,
    palette: Option<Vec<String>>,
    glow: Option<String>,
    floor_bg: Option<String>,
    wall_bg: Option<String>,
}

/// Per-theme colour transform (tint then palette remap).
#[derive(Debug, Clone, Default)]
pub struct Fx {
    pub tint: Option<Col>,
    pub palette: Vec<Col>,
}

impl Fx {
    pub fn is_identity(&self) -> bool {
        self.tint.is_none() && self.palette.is_empty()
    }
    pub fn apply(&self, c: Col) -> Col {
        let mut c = c;
        if let Some(t) = self.tint {
            c = tint(c, t);
        }
        if !self.palette.is_empty() {
            let l = (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0;
            let n = self.palette.len();
            let idx = ((l * n as f32) as usize).min(n - 1);
            let p = self.palette[idx];
            c = [p[0], p[1], p[2], c[3]];
        }
        c
    }
}

fn tint(c: Col, t: Col) -> Col {
    [
        (c[0] as u16 * t[0] as u16 / 255) as u8,
        (c[1] as u16 * t[1] as u16 / 255) as u8,
        (c[2] as u16 * t[2] as u16 / 255) as u8,
        c[3],
    ]
}

#[derive(Debug, Clone)]
pub struct Sprite {
    pub frames: Vec<RgbaImage>,
    /// Screen rect inside a monitor sprite (x, y, w, h).
    pub screen: Option<[i32; 4]>,
    /// True if it came from the procedural fallback.
    pub fallback: bool,
}

impl Sprite {
    pub fn frame(&self, i: usize) -> &RgbaImage {
        &self.frames[i % self.frames.len()]
    }
    pub fn w(&self) -> i32 {
        self.frames[0].width() as i32
    }
    pub fn h(&self) -> i32 {
        self.frames[0].height() as i32
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub sprites: HashMap<String, Sprite>,
    pub fx: Fx,
    pub colors: Colors,
    pub glow: Option<Col>,
    pub floor_bg: Option<Col>,
    pub wall_bg: Option<Col>,
}

impl Theme {
    pub fn sprite(&self, name: &str) -> &Sprite {
        self.sprites.get(name).unwrap_or_else(|| panic!("theme {} lacks {name}", self.name))
    }
}

pub fn procedural() -> Theme {
    build("procedural", HashMap::new(), Colors::default(), Fx::default(), None, None, None)
}

fn build(
    name: &str,
    mut sprites: HashMap<String, Sprite>,
    colors: Colors,
    fx: Fx,
    glow: Option<Col>,
    floor_bg: Option<Col>,
    wall_bg: Option<Col>,
) -> Theme {
    for (k, (img, screen)) in procedural::all(&colors) {
        sprites.entry(k.to_string()).or_insert_with(|| Sprite { frames: vec![img], screen, fallback: true });
    }
    if !fx.is_identity() {
        for s in sprites.values_mut() {
            for f in &mut s.frames {
                map_pixels(f, &|c| fx.apply(c));
            }
        }
    }
    Theme { name: name.to_string(), sprites, fx, colors, glow, floor_bg, wall_bg }
}

/// Parse a theme.toml (sheets resolved relative to `dir`).
pub fn parse(name: &str, src: &str, dir: &Path) -> Result<Theme> {
    let tf: ThemeFile = toml::from_str(src).with_context(|| format!("parse {name}/theme.toml"))?;
    let mut sheets: HashMap<String, (RgbaImage, &SheetDef)> = HashMap::new();
    for (k, sd) in &tf.sheets {
        let p = dir.join(&sd.file);
        let mut img = image::open(&p).with_context(|| format!("open {}", p.display()))?.to_rgba8();
        if let Some(key) = sd.key.as_deref().and_then(parse_hex) {
            for px in img.pixels_mut() {
                if px.0[..3] == key[..3] {
                    px.0[3] = 0;
                }
            }
        }
        sheets.insert(k.clone(), (img, sd));
    }
    let mut sprites = HashMap::new();
    for (k, def) in &tf.sprites {
        let (sheet, sd) = sheets.get(&def.sheet).ok_or_else(|| anyhow!("{name}: sprite {k}: unknown sheet {}", def.sheet))?;
        let pitch = sd.tile + sd.spacing;
        let nframes = def.frames.unwrap_or(1).max(1);
        let mut frames = Vec::new();
        for f in 0..nframes {
            let img = if let Some(tiles) = &def.tiles {
                let [gc, gr] = def.grid.unwrap_or([tiles.len() as u32, 1]);
                let mut out = RgbaImage::new(gc * sd.tile, gr * sd.tile);
                for (i, [c, r]) in tiles.iter().enumerate() {
                    let t = crop(sheet, (c + f * gc) * pitch, r * pitch, sd.tile, sd.tile);
                    let (ox, oy) = ((i as u32 % gc) * sd.tile, (i as u32 / gc) * sd.tile);
                    image::imageops::overlay(&mut out, &t, ox as i64, oy as i64);
                }
                out
            } else if let (Some(c), Some(r)) = (def.col, def.row) {
                let (cw, ch) = (def.cols.unwrap_or(1), def.rows.unwrap_or(1));
                let mut out = RgbaImage::new(cw * sd.tile, ch * sd.tile);
                for ty in 0..ch {
                    for tx in 0..cw {
                        let t = crop(sheet, (c + tx + f * cw) * pitch, (r + ty) * pitch, sd.tile, sd.tile);
                        image::imageops::overlay(&mut out, &t, (tx * sd.tile) as i64, (ty * sd.tile) as i64);
                    }
                }
                out
            } else {
                let (x, y) = (def.x.unwrap_or(0), def.y.unwrap_or(0));
                let (w, h) = (def.w.unwrap_or(sd.tile), def.h.unwrap_or(sd.tile));
                crop(sheet, x + f * (w + sd.spacing), y, w, h)
            };
            let mut img = img;
            if let Some(t) = def.tint.as_deref().and_then(parse_hex) {
                map_pixels(&mut img, &|c| tint(c, t));
            }
            frames.push(img);
        }
        sprites.insert(k.clone(), Sprite { frames, screen: def.screen, fallback: false });
    }
    let mut colors = Colors::default();
    for (k, v) in &tf.colors {
        colors.set(k, v);
    }
    let fx = Fx {
        tint: tf.fx.tint.as_deref().and_then(parse_hex),
        palette: tf.fx.palette.iter().flatten().filter_map(|s| parse_hex(s)).collect(),
    };
    let ph = |o: &Option<String>| o.as_deref().and_then(parse_hex);
    Ok(build(name, sprites, colors, fx, ph(&tf.fx.glow), ph(&tf.fx.floor_bg), ph(&tf.fx.wall_bg)))
}

/// Directories searched for theme packs, in priority order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = office_core::paths::theme_dirs();
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes"));
    v
}

/// Theme names found on disk plus "procedural" (first), deduplicated. Templates (`*.example`)
/// are skipped.
pub fn list(dirs: &[PathBuf]) -> Vec<String> {
    let mut names = vec!["procedural".to_string()];
    let mut found = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            if e.path().join("theme.toml").is_file() {
                if let Some(n) = e.file_name().to_str().filter(|n| !n.ends_with(".example")) {
                    found.push(n.to_string());
                }
            }
        }
    }
    found.sort();
    for n in found {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

pub fn load(name: &str, dirs: &[PathBuf]) -> Result<Theme> {
    if name == "procedural" {
        return Ok(procedural());
    }
    for d in dirs {
        let p = d.join(name).join("theme.toml");
        if p.is_file() {
            let src = std::fs::read_to_string(&p)?;
            return parse(name, &src, &d.join(name));
        }
    }
    Err(anyhow!("theme {name:?} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("office-theme-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn procedural_has_every_sprite() {
        let t = procedural();
        for n in SPRITE_NAMES {
            assert!(t.sprites.contains_key(n), "missing {n}");
        }
        assert!(t.sprite("monitor_on").screen.is_some());
    }

    #[test]
    fn parses_sheet_tiles_fx_and_fallbacks() {
        let d = tmpdir("parse");
        // 2×2 tiles of 4 px with 1 px spacing: 9×9 sheet
        let mut sheet = RgbaImage::new(9, 9);
        for p in sheet.pixels_mut() {
            p.0 = [255, 0, 255, 255];
        }
        for y in 0..4 {
            for x in 0..4 {
                sheet.put_pixel(x, y, image::Rgba([255, 255, 255, 255])); // tile (0,0)
                sheet.put_pixel(x + 5, y, image::Rgba([0, 0, 0, 255])); // tile (1,0)
            }
        }
        sheet.save(d.join("s.png")).unwrap();
        let src = r##"
            [sheets]
            s = { file = "s.png", tile = 4, spacing = 1, key = "#ff00ff" }
            [sprites]
            floor = { sheet = "s", col = 0, row = 0 }
            desk = { sheet = "s", tiles = [[0,0],[1,0]] }
            wall = { sheet = "s", x = 0, y = 0, w = 4, h = 4, frames = 2 }
            plant = { sheet = "s", col = 0, row = 1 }
            [colors]
            floor_style = "wood"
            [fx]
            palette = ["#000000", "#00ff00"]
            glow = "#00ffff"
        "##;
        let t = parse("t", src, &d).unwrap();
        let floor = t.sprite("floor");
        assert!(!floor.fallback);
        assert_eq!(floor.frames[0].dimensions(), (4, 4));
        assert_eq!(floor.frames[0].get_pixel(0, 0).0, [0, 255, 0, 255], "white -> palette[1]");
        let desk = t.sprite("desk");
        assert_eq!(desk.frames[0].dimensions(), (8, 4));
        assert_eq!(desk.frames[0].get_pixel(5, 0).0, [0, 0, 0, 255]);
        assert_eq!(t.sprite("wall").frames.len(), 2);
        // colour-keyed magenta became transparent
        assert_eq!(t.sprite("plant").frames[0].get_pixel(0, 0).0[3], 0);
        // missing sprites come from procedural
        assert!(t.sprite("sofa").fallback);
        assert_eq!(t.glow, Some([0, 255, 255, 255]));
        assert_eq!(t.colors.floor_style, "wood");
    }

    #[test]
    fn unknown_sheet_is_an_error() {
        let d = tmpdir("bad");
        let src = "[sprites]\nfloor = { sheet = \"nope\", col = 0, row = 0 }\n";
        assert!(parse("bad", src, &d).is_err());
    }

    #[test]
    fn bundled_themes_load() {
        let dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes")];
        let names = list(&dirs);
        assert_eq!(names[0], "procedural");
        for n in ["modern", "cozy", "retro", "cyberpunk"] {
            assert!(names.iter().any(|x| x == n), "{n} not listed: {names:?}");
            let t = load(n, &dirs).unwrap_or_else(|e| panic!("{n}: {e:#}"));
            for s in SPRITE_NAMES {
                assert!(t.sprites.contains_key(s));
            }
            assert!(t.sprites.values().any(|s| !s.fallback), "{n} uses no sheet sprites");
        }
        assert!(load("does-not-exist", &dirs).is_err());
    }

    #[test]
    fn premium_template_uses_the_loader_format() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/premium.example");
        let src = std::fs::read_to_string(dir.join("theme.toml")).unwrap();
        let tf: ThemeFile = toml::from_str(&src).expect("template parses as a theme.toml");
        assert!(!tf.sheets.is_empty() && !tf.sprites.is_empty());
        for (k, def) in &tf.sprites {
            assert!(tf.sheets.contains_key(&def.sheet), "sprite {k}: unknown sheet {}", def.sheet);
            let region = def.tiles.is_some() || (def.col.is_some() && def.row.is_some()) || def.w.is_some();
            assert!(region, "sprite {k} names no region");
        }
        for n in SPRITE_NAMES {
            assert!(tf.sprites.contains_key(n), "template maps {n}");
        }
        // a template is not a theme: it never shows up in the theme cycle
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
        assert!(!list(&[root]).iter().any(|n| n.ends_with(".example")));
    }
}
