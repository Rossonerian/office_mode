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
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub use office_core::assets::SPRITE_NAMES;

pub const MAX_TOML_BYTES: usize = 256 * 1024;
pub const MAX_FRAMES: u32 = 32;
pub const MAX_THEME_PIXELS: u64 = 16_000_000;
pub const MAX_PNG_DIM: u32 = 4096;
pub const MAX_PNG_BYTES: u64 = 8 * 1024 * 1024;

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
    char_tint: Option<String>,
    palette: Option<Vec<String>>,
    glow: Option<String>,
    floor_bg: Option<String>,
    wall_bg: Option<String>,
}

/// Per-theme colour transform (tint then palette remap).
#[derive(Debug, Clone, Default)]
pub struct Fx {
    pub tint: Option<Col>,
    pub char_tint: Option<Col>,
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
    pub fn apply_char(&self, c: Col) -> Col {
        let c = self.apply(c);
        match self.char_tint {
            Some(ct) if self.palette.is_empty() => tint(c, ct),
            _ => c,
        }
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
    pub fn sprite_opt(&self, name: &str) -> Option<&Sprite> {
        self.sprites.get(name)
    }
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

/// Resolve an atlas path relative to the theme dir with a configurable maximum parent-directory ascent.
/// Absolute paths are rejected, and `..` may climb at most `max_ups` levels above `dir`.
pub fn resolve_sheet_path_with_max_ups(dir: &Path, file: &str, max_ups: usize) -> Result<PathBuf> {
    use std::path::Component;
    let rel = Path::new(file);
    let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
    let mut ups = 0usize;
    for c in rel.components() {
        match c {
            Component::Normal(n) => stack.push(n),
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    ups += 1;
                    if ups > max_ups {
                        return Err(anyhow!("atlas path {file:?} escapes the allowed directory"));
                    }
                }
            }
            _ => return Err(anyhow!("atlas path {file:?} must be relative")),
        }
    }
    if stack.is_empty() {
        return Err(anyhow!("atlas path {file:?} names no file"));
    }
    Ok(dir.join(rel))
}

/// Resolve an atlas path relative to the theme dir. Absolute paths are rejected, and `..` may climb at
/// most to the assets root (two levels above the theme dir, e.g. `../../vendor/pack/sheet.png`).
pub fn resolve_sheet_path(dir: &Path, file: &str) -> Result<PathBuf> {
    resolve_sheet_path_with_max_ups(dir, file, 2)
}

fn add_pixels(total: &mut u64, w: u32, h: u32, ctx: &str) -> Result<()> {
    let px = (w as u64)
        .checked_mul(h as u64)
        .ok_or_else(|| anyhow!("{ctx}: pixel count overflow ({w}x{h})"))?;
    let next = total
        .checked_add(px)
        .ok_or_else(|| anyhow!("{ctx}: total pixel count overflow"))?;
    if next > MAX_THEME_PIXELS {
        return Err(anyhow!(
            "{ctx}: theme pixel budget exceeded ({next} > {MAX_THEME_PIXELS})"
        ));
    }
    *total = next;
    Ok(())
}

fn open_sheet_image(dir: &Path, file: &str, max_ups: usize) -> Result<RgbaImage> {
    let p = resolve_sheet_path_with_max_ups(dir, file, max_ups)?;
    let meta = std::fs::symlink_metadata(&p).with_context(|| format!("stat {}", p.display()))?;
    if meta.file_type().is_symlink() {
        return Err(anyhow!("refusing symlink sheet: {}", p.display()));
    }
    if !meta.is_file() {
        return Err(anyhow!("sheet is not a regular file: {}", p.display()));
    }
    if meta.len() > MAX_PNG_BYTES {
        return Err(anyhow!(
            "sheet {} ({} bytes) exceeds limit {MAX_PNG_BYTES} bytes",
            p.display(),
            meta.len()
        ));
    }
    let allowed_root = dir
        .ancestors()
        .nth(max_ups)
        .ok_or_else(|| anyhow!("cannot compute allowed root for {}", dir.display()))?;
    let canon_root = std::fs::canonicalize(allowed_root)
        .with_context(|| format!("canonicalize {}", allowed_root.display()))?;
    let canon_p = std::fs::canonicalize(&p).with_context(|| format!("canonicalize {}", p.display()))?;
    if !canon_p.starts_with(&canon_root) {
        return Err(anyhow!(
            "sheet {} escapes allowed root {}",
            canon_p.display(),
            canon_root.display()
        ));
    }
    let mut reader = image::ImageReader::open(&canon_p).with_context(|| format!("open {}", p.display()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PNG_DIM);
    limits.max_image_height = Some(MAX_PNG_DIM);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().with_context(|| format!("decode {}", p.display()))?.to_rgba8();
    Ok(img)
}

/// Parse a theme.toml (sheets resolved relative to `dir`, allowing up to 2 parent hops).
pub fn parse(name: &str, src: &str, dir: &Path) -> Result<Theme> {
    parse_with_max_ups(name, src, dir, 2)
}

/// Parse a theme.toml with an explicit maximum parent-directory ascent (`0` for user themes, `2` for bundled).
pub fn parse_with_max_ups(name: &str, src: &str, dir: &Path, max_ups: usize) -> Result<Theme> {
    if src.len() > MAX_TOML_BYTES {
        return Err(anyhow!(
            "{name}/theme.toml ({} bytes) exceeds limit {MAX_TOML_BYTES} bytes",
            src.len()
        ));
    }
    let tf: ThemeFile = toml::from_str(src).with_context(|| format!("parse {name}/theme.toml"))?;
    let mut total_pixels = 0u64;
    let mut sheets: HashMap<String, (RgbaImage, &SheetDef)> = HashMap::new();
    for (k, sd) in &tf.sheets {
        let mut img = open_sheet_image(dir, &sd.file, max_ups).with_context(|| format!("{name}: sheet {k}"))?;
        add_pixels(&mut total_pixels, img.width(), img.height(), &format!("{name}: sheet {k}"))?;
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
        let ctx = format!("{name}: sprite {k}");
        let (sheet, sd) = sheets.get(&def.sheet).ok_or_else(|| anyhow!("{ctx}: unknown sheet {}", def.sheet))?;
        let pitch = sd.tile.checked_add(sd.spacing).ok_or_else(|| anyhow!("{ctx}: tile+spacing overflow"))?;
        let nframes = def.frames.unwrap_or(1);
        if nframes == 0 || nframes > MAX_FRAMES {
            return Err(anyhow!("{ctx}: frames must be in 1..={MAX_FRAMES}, got {nframes}"));
        }
        let mut frames = Vec::with_capacity(nframes as usize);
        for f in 0..nframes {
            let img = if let Some(tiles) = &def.tiles {
                if sd.tile == 0 {
                    return Err(anyhow!("{ctx}: sheet tile size must be > 0"));
                }
                let default_gc = u32::try_from(tiles.len()).map_err(|_| anyhow!("{ctx}: too many tiles"))?;
                let [gc, gr] = def.grid.unwrap_or([default_gc, 1]);
                if gc == 0 || gr == 0 {
                    return Err(anyhow!("{ctx}: grid dimensions must be > 0, got [{gc}, {gr}]"));
                }
                let out_w = gc.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: sprite width overflow"))?;
                let out_h = gr.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: sprite height overflow"))?;
                if out_w > MAX_PNG_DIM || out_h > MAX_PNG_DIM {
                    return Err(anyhow!("{ctx}: sprite dimensions {out_w}x{out_h} exceed {MAX_PNG_DIM}"));
                }
                add_pixels(&mut total_pixels, out_w, out_h, &ctx)?;
                let mut out = RgbaImage::new(out_w, out_h);
                for (i, [c, r]) in tiles.iter().enumerate() {
                    let i32_idx = u32::try_from(i).map_err(|_| anyhow!("{ctx}: tile index overflow"))?;
                    let sx = f
                        .checked_mul(gc)
                        .and_then(|v| c.checked_add(v))
                        .and_then(|v| v.checked_mul(pitch))
                        .ok_or_else(|| anyhow!("{ctx}: source x overflow"))?;
                    let sy = r.checked_mul(pitch).ok_or_else(|| anyhow!("{ctx}: source y overflow"))?;
                    let ox = (i32_idx % gc).checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: dest x overflow"))?;
                    let oy = (i32_idx / gc).checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: dest y overflow"))?;
                    let t = crop(sheet, sx, sy, sd.tile, sd.tile);
                    image::imageops::overlay(&mut out, &t, ox as i64, oy as i64);
                }
                out
            } else if let (Some(c), Some(r)) = (def.col, def.row) {
                if sd.tile == 0 {
                    return Err(anyhow!("{ctx}: sheet tile size must be > 0"));
                }
                let (cw, ch) = (def.cols.unwrap_or(1), def.rows.unwrap_or(1));
                if cw == 0 || ch == 0 {
                    return Err(anyhow!("{ctx}: cols/rows must be > 0, got {cw}x{ch}"));
                }
                let out_w = cw.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: sprite width overflow"))?;
                let out_h = ch.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: sprite height overflow"))?;
                if out_w > MAX_PNG_DIM || out_h > MAX_PNG_DIM {
                    return Err(anyhow!("{ctx}: sprite dimensions {out_w}x{out_h} exceed {MAX_PNG_DIM}"));
                }
                add_pixels(&mut total_pixels, out_w, out_h, &ctx)?;
                let mut out = RgbaImage::new(out_w, out_h);
                for ty in 0..ch {
                    for tx in 0..cw {
                        let sx = f
                            .checked_mul(cw)
                            .and_then(|v| c.checked_add(tx)?.checked_add(v))
                            .and_then(|v| v.checked_mul(pitch))
                            .ok_or_else(|| anyhow!("{ctx}: source x overflow"))?;
                        let sy = r
                            .checked_add(ty)
                            .and_then(|v| v.checked_mul(pitch))
                            .ok_or_else(|| anyhow!("{ctx}: source y overflow"))?;
                        let ox = tx.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: dest x overflow"))?;
                        let oy = ty.checked_mul(sd.tile).ok_or_else(|| anyhow!("{ctx}: dest y overflow"))?;
                        let t = crop(sheet, sx, sy, sd.tile, sd.tile);
                        image::imageops::overlay(&mut out, &t, ox as i64, oy as i64);
                    }
                }
                out
            } else {
                let (x, y) = (def.x.unwrap_or(0), def.y.unwrap_or(0));
                let (w, h) = (def.w.unwrap_or(sd.tile), def.h.unwrap_or(sd.tile));
                if w == 0 || h == 0 {
                    return Err(anyhow!("{ctx}: sprite dimensions must be > 0, got {w}x{h}"));
                }
                if w > MAX_PNG_DIM || h > MAX_PNG_DIM {
                    return Err(anyhow!("{ctx}: sprite dimensions {w}x{h} exceed {MAX_PNG_DIM}"));
                }
                add_pixels(&mut total_pixels, w, h, &ctx)?;
                let stride = w.checked_add(sd.spacing).ok_or_else(|| anyhow!("{ctx}: stride overflow"))?;
                let sx = f
                    .checked_mul(stride)
                    .and_then(|v| x.checked_add(v))
                    .ok_or_else(|| anyhow!("{ctx}: source x overflow"))?;
                crop(sheet, sx, y, w, h)
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
        char_tint: tf.fx.char_tint.as_deref().and_then(parse_hex),
        palette: tf.fx.palette.iter().flatten().filter_map(|s| parse_hex(s)).collect(),
    };
    let ph = |o: &Option<String>| o.as_deref().and_then(parse_hex);
    Ok(build(name, sprites, colors, fx, ph(&tf.fx.glow), ph(&tf.fx.floor_bg), ph(&tf.fx.wall_bg)))
}

/// Directories searched for theme packs, in priority order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = office_core::paths::theme_dirs();
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    if !v.contains(&dev) {
        v.push(dev);
    }
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

fn is_inside_user_themes(dir: &Path, user_themes: &Path, user_themes_canon: Option<&Path>) -> bool {
    if dir == user_themes || dir.starts_with(user_themes) {
        return true;
    }
    if let Some(uc) = user_themes_canon {
        if let Ok(dc) = std::fs::canonicalize(dir) {
            return dc == uc || dc.starts_with(uc);
        }
    }
    false
}

pub fn load(name: &str, dirs: &[PathBuf]) -> Result<Theme> {
    if name == "procedural" {
        return Ok(procedural());
    }
    office_core::assets::sanitize_pack_name(name).map_err(|e| anyhow!("{e}"))?;
    let user_themes = office_core::paths::user_themes_dir();
    let user_themes_canon = std::fs::canonicalize(&user_themes).ok();
    for d in dirs {
        let theme_dir = d.join(name);
        let dir_meta = match std::fs::symlink_metadata(&theme_dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow!("stat {}: {e}", theme_dir.display())),
        };
        if dir_meta.file_type().is_symlink() {
            return Err(anyhow!("refusing symlink theme dir: {}", theme_dir.display()));
        }
        if !dir_meta.is_dir() {
            continue;
        }
        let p = theme_dir.join("theme.toml");
        let toml_meta = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow!("stat {}: {e}", p.display())),
        };
        if toml_meta.file_type().is_symlink() {
            return Err(anyhow!("refusing symlink theme.toml: {}", p.display()));
        }
        if !toml_meta.is_file() {
            continue;
        }
        if toml_meta.len() > MAX_TOML_BYTES as u64 {
            return Err(anyhow!(
                "{} ({} bytes) exceeds limit {MAX_TOML_BYTES} bytes",
                p.display(),
                toml_meta.len()
            ));
        }
        let f = File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let mut bytes = Vec::with_capacity((toml_meta.len() as usize).min(MAX_TOML_BYTES));
        f.take((MAX_TOML_BYTES as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("read {}", p.display()))?;
        if bytes.len() > MAX_TOML_BYTES {
            return Err(anyhow!("{} exceeds limit {MAX_TOML_BYTES} bytes", p.display()));
        }
        let src = std::str::from_utf8(&bytes).with_context(|| format!("{} is not valid UTF-8", p.display()))?;
        let is_user_dir = is_inside_user_themes(d, &user_themes, user_themes_canon.as_deref());
        let max_ups = if is_user_dir { 0 } else { 2 };
        return parse_with_max_ups(name, src, &theme_dir, max_ups);
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
        for n in ["modern", "cozy", "retro", "cyberpunk", "tokyo-night", "nord", "retro-1bit", "retro-amber", "retro-gameboy"] {
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

    #[test]
    fn sheet_paths_are_confined() {
        let d = Path::new("/x/assets/themes/t");
        assert!(resolve_sheet_path(d, "a.png").is_ok());
        assert!(resolve_sheet_path(d, "../../vendor/p/a.png").is_ok());
        assert!(resolve_sheet_path(d, "../../../etc/a.png").is_err());
        assert!(resolve_sheet_path(d, "/etc/passwd").is_err());
    }

    /// Every atlas of every bundled theme resolves inside assets/ and every sprite rectangle is in bounds.
    #[test]
    fn bundled_theme_atlases_resolve_and_rects_in_bounds() {
        let assets = std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")).unwrap();
        let themes = assets.join("themes");
        for e in std::fs::read_dir(&themes).unwrap().flatten() {
            let dir = e.path();
            let tp = dir.join("theme.toml");
            if !tp.is_file() {
                continue;
            }
            let tn = e.file_name().to_string_lossy().to_string();
            if tn.ends_with(".example") {
                continue; // templates reference premium art that is not bundled
            }
            let tf: ThemeFile = toml::from_str(&std::fs::read_to_string(&tp).unwrap()).unwrap_or_else(|e| panic!("{tn}: {e}"));
            let mut dims: HashMap<&String, (u32, u32, u32, u32)> = HashMap::new();
            for (k, sd) in &tf.sheets {
                let p = resolve_sheet_path(&dir, &sd.file).unwrap_or_else(|e| panic!("{tn}/{k}: {e:#}"));
                let real = std::fs::canonicalize(&p).unwrap_or_else(|e| panic!("{tn}/{k}: {} {e}", p.display()));
                assert!(real.starts_with(&assets), "{tn}/{k}: {} outside assets", real.display());
                let (w, h) = image::image_dimensions(&real).unwrap();
                dims.insert(k, (w, h, sd.tile, sd.spacing));
            }
            for (k, def) in &tf.sprites {
                let (w, h, tile, sp) = *dims.get(&def.sheet).unwrap_or_else(|| panic!("{tn}/{k}: unknown sheet"));
                let pitch = tile + sp;
                let nf = def.frames.unwrap_or(1).max(1);
                let ok = |x: u32, y: u32, rw: u32, rh: u32| {
                    assert!(x + rw <= w && y + rh <= h, "{tn}/{k}: rect {x},{y} {rw}x{rh} outside {w}x{h} sheet");
                };
                for f in 0..nf {
                    if let Some(tiles) = &def.tiles {
                        let gc = def.grid.map_or(tiles.len() as u32, |g| g[0]);
                        for [c, r] in tiles {
                            ok((c + f * gc) * pitch, r * pitch, tile, tile);
                        }
                    } else if let (Some(c), Some(r)) = (def.col, def.row) {
                        let (cw, ch) = (def.cols.unwrap_or(1), def.rows.unwrap_or(1));
                        ok((c + f * cw) * pitch, r * pitch, cw * tile + (cw - 1) * sp, ch * tile + (ch - 1) * sp);
                    } else {
                        let (rw, rh) = (def.w.unwrap_or(tile), def.h.unwrap_or(tile));
                        ok(def.x.unwrap_or(0) + f * (rw + sp), def.y.unwrap_or(0), rw, rh);
                    }
                }
            }
        }
    }

    #[test]
    fn load_rejects_path_traversal_and_bad_names() {
        let d = tmpdir("traversal");
        assert!(load("../evil", std::slice::from_ref(&d)).is_err());
        assert!(load("a/b", std::slice::from_ref(&d)).is_err());
        assert!(load("..\\", std::slice::from_ref(&d)).is_err());
        assert!(load("Upper", std::slice::from_ref(&d)).is_err());
    }

    #[test]
    fn parse_rejects_zero_tile_zero_grid_and_excess_frames() {
        let d = tmpdir("limits");
        let sheet = RgbaImage::new(16, 16);
        sheet.save(d.join("s.png")).unwrap();

        let zero_tile = r##"
            [sheets]
            s = { file = "s.png", tile = 0 }
            [sprites]
            floor = { sheet = "s", col = 0, row = 0 }
        "##;
        assert!(parse("zt", zero_tile, &d).is_err());

        let zero_grid = r##"
            [sheets]
            s = { file = "s.png", tile = 4 }
            [sprites]
            desk = { sheet = "s", tiles = [[0, 0]], grid = [0, 1] }
        "##;
        assert!(parse("zg", zero_grid, &d).is_err());

        let too_many_frames = r##"
            [sheets]
            s = { file = "s.png", tile = 4 }
            [sprites]
            wall = { sheet = "s", col = 0, row = 0, frames = 99 }
        "##;
        assert!(parse("mf", too_many_frames, &d).is_err());

        let zero_frames = r##"
            [sheets]
            s = { file = "s.png", tile = 4 }
            [sprites]
            wall = { sheet = "s", col = 0, row = 0, frames = 0 }
        "##;
        assert!(parse("zf", zero_frames, &d).is_err());

        // User themes (max_ups = 0) cannot escape their own directory.
        assert!(resolve_sheet_path_with_max_ups(&d, "../s.png", 0).is_err());
    }

    #[test]
    fn load_and_parse_reject_oversize_toml() {
        let root = tmpdir("oversize");
        let pack = root.join("bigpack");
        std::fs::create_dir_all(&pack).unwrap();
        let big = format!("# {}\n", "x".repeat(MAX_TOML_BYTES + 64));
        std::fs::write(pack.join("theme.toml"), &big).unwrap();

        assert!(parse("bigpack", &big, &pack).is_err());
        assert!(load("bigpack", &[root]).is_err());
    }
}
