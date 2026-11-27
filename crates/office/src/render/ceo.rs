//! The CEO (you): an original procedural sprite (18×28 at 1×, a bit larger than the agents) in a
//! football jersey with vertical red and black stripes (no crest, logo or sponsor), black shorts
//! and socks, and an optional number on the back. Its mood mirrors the office: it turns toward a
//! NeedsYou alert, reviews papers and nods while agents work, and sips coffee or leans back when
//! everything is idle.
use super::canvas::{fill, parse_hex, put, rgb, shade, Col};
use super::font;
use image::RgbaImage;
use office_core::{assets::OutfitManifest, config::CeoConfig, AgentState, Scene};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const W: i32 = 18;
pub const H: i32 = 28;

const RED: Col = rgb(0xd0202a);
const BLACK: Col = rgb(0x18181b);
const MAX_OUTFIT_TOML_BYTES: usize = 256 * 1024;
const MAX_OUTFIT_PNG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_OUTFIT_PNG_DIM: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutfitMode {
    Torso,
    Full,
}

#[derive(Debug, Clone)]
pub struct CeoOutfit {
    pub mode: OutfitMode,
    pub sprite: RgbaImage,
}

fn parse_outfit_mode(raw: &str) -> Option<OutfitMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "full" => Some(OutfitMode::Full),
        "torso" => Some(OutfitMode::Torso),
        _ => None,
    }
}

fn validate_outfit_rel_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("outfit sprite path is empty".into());
    }
    let mut out = PathBuf::new();
    let mut has_normal = false;
    for comp in Path::new(raw).components() {
        match comp {
            Component::Normal(p) => {
                out.push(p);
                has_normal = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("outfit sprite path {raw:?} escapes outfit directory"));
            }
        }
    }
    if !has_normal {
        return Err(format!("outfit sprite path {raw:?} names no file"));
    }
    Ok(out)
}

/// Load an optional external CEO outfit sprite from `<outfits_root>/<name>`.
pub fn load_outfit(cfg: &CeoConfig, outfits_root: &Path) -> Result<Option<CeoOutfit>, String> {
    let Some(raw_name) = cfg.outfit.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let name = office_core::assets::sanitize_pack_name(raw_name)?;
    let outfit_dir = outfits_root.join(&name);

    let dir_meta = std::fs::symlink_metadata(&outfit_dir)
        .map_err(|e| format!("cannot stat outfit dir {}: {e}", outfit_dir.display()))?;
    if dir_meta.file_type().is_symlink() {
        return Err(format!("refusing symlink outfit dir: {}", outfit_dir.display()));
    }
    if !dir_meta.is_dir() {
        return Err(format!("outfit path is not a directory: {}", outfit_dir.display()));
    }

    let toml_path = outfit_dir.join("outfit.toml");
    let toml_meta = std::fs::symlink_metadata(&toml_path)
        .map_err(|e| format!("cannot stat {}: {e}", toml_path.display()))?;
    if toml_meta.file_type().is_symlink() {
        return Err(format!("refusing symlink outfit.toml: {}", toml_path.display()));
    }
    if !toml_meta.is_file() {
        return Err(format!("outfit.toml is not a regular file: {}", toml_path.display()));
    }
    if toml_meta.len() > MAX_OUTFIT_TOML_BYTES as u64 {
        return Err(format!(
            "outfit.toml ({} bytes) exceeds limit {MAX_OUTFIT_TOML_BYTES} bytes",
            toml_meta.len()
        ));
    }
    let f = File::open(&toml_path).map_err(|e| format!("cannot open {}: {e}", toml_path.display()))?;
    let mut toml_bytes = Vec::with_capacity((toml_meta.len() as usize).min(MAX_OUTFIT_TOML_BYTES));
    f.take((MAX_OUTFIT_TOML_BYTES as u64).saturating_add(1))
        .read_to_end(&mut toml_bytes)
        .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
    if toml_bytes.len() > MAX_OUTFIT_TOML_BYTES {
        return Err(format!("outfit.toml exceeds limit {MAX_OUTFIT_TOML_BYTES} bytes"));
    }
    let toml_str = std::str::from_utf8(&toml_bytes).map_err(|e| format!("outfit.toml is not UTF-8: {e}"))?;
    let manifest: OutfitManifest = toml::from_str(toml_str).map_err(|e| format!("invalid outfit.toml: {e}"))?;

    let rel_sprite = validate_outfit_rel_path(&manifest.sprite)?;
    let sprite_path = outfit_dir.join(&rel_sprite);
    let sprite_meta = std::fs::symlink_metadata(&sprite_path)
        .map_err(|e| format!("cannot stat outfit sprite {}: {e}", sprite_path.display()))?;
    if sprite_meta.file_type().is_symlink() {
        return Err(format!("refusing symlink outfit sprite: {}", sprite_path.display()));
    }
    if !sprite_meta.is_file() {
        return Err(format!("outfit sprite is not a regular file: {}", sprite_path.display()));
    }
    if sprite_meta.len() > MAX_OUTFIT_PNG_BYTES {
        return Err(format!(
            "outfit sprite ({} bytes) exceeds limit {MAX_OUTFIT_PNG_BYTES} bytes",
            sprite_meta.len()
        ));
    }

    let canon_dir = std::fs::canonicalize(&outfit_dir)
        .map_err(|e| format!("cannot canonicalize {}: {e}", outfit_dir.display()))?;
    let canon_sprite = std::fs::canonicalize(&sprite_path)
        .map_err(|e| format!("cannot canonicalize {}: {e}", sprite_path.display()))?;
    if !canon_sprite.starts_with(&canon_dir) {
        return Err(format!(
            "outfit sprite {} escapes {}",
            canon_sprite.display(),
            canon_dir.display()
        ));
    }

    let mut reader = image::ImageReader::open(&canon_sprite)
        .map_err(|e| format!("cannot open outfit sprite {}: {e}", canon_sprite.display()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_OUTFIT_PNG_DIM);
    limits.max_image_height = Some(MAX_OUTFIT_PNG_DIM);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let sprite = reader
        .decode()
        .map_err(|e| format!("cannot decode outfit sprite {}: {e}", canon_sprite.display()))?
        .to_rgba8();

    let mode = parse_outfit_mode(&cfg.outfit_mode)
        .or_else(|| manifest.mode.as_deref().and_then(parse_outfit_mode))
        .unwrap_or(OutfitMode::Torso);

    Ok(Some(CeoOutfit { mode, sprite }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HairStyle {
    Short,
    Curly,
    Long,
    Bald,
}

#[derive(Debug, Clone)]
pub struct CeoLook {
    pub skin: Col,
    pub hair: Col,
    pub style: HairStyle,
    pub number: u32,
    pub primary: Col,
    pub secondary: Col,
}

pub fn look(c: &CeoConfig) -> CeoLook {
    let hair = match c.hair.trim().to_ascii_lowercase().as_str() {
        "black" => rgb(0x1c1a1a),
        "brown" => rgb(0x5a3a22),
        "blonde" | "blond" => rgb(0xd8b45a),
        "red" | "ginger" => rgb(0xb33a2a),
        "grey" | "gray" => rgb(0x9aa0a8),
        "white" => rgb(0xeeeeee),
        other => parse_hex(other).unwrap_or(rgb(0x1c1a1a)),
    };
    let style = match c.hair_style.trim().to_ascii_lowercase().as_str() {
        "curly" => HairStyle::Curly,
        "long" => HairStyle::Long,
        "bald" => HairStyle::Bald,
        _ => HairStyle::Short,
    };
    let primary = c.primary_color.as_deref().and_then(parse_hex).unwrap_or(RED);
    let secondary = c.secondary_color.as_deref().and_then(parse_hex).unwrap_or(BLACK);
    CeoLook { skin: parse_hex(&c.skin).unwrap_or(rgb(0xc68642)), hair, style, number: c.number.min(99), primary, secondary }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    /// Someone needs you: stand up, look/point toward them (`left` = the alert is to the left).
    Alert { left: bool },
    /// Agents are working: review papers and nod.
    Review { nod: bool },
    /// Idle: coffee (sip = mug at the mouth).
    Coffee { sip: bool },
    /// Idle: lean back, hands behind the head.
    LeanBack,
    /// Idle: turned around looking out (shows the number on the back).
    Back,
}

impl Mood {
    pub fn standing(&self) -> bool {
        matches!(self, Mood::Alert { .. } | Mood::Back)
    }
}

/// Mood from the scene. `alert_x` maps an agent id to its x in the room (for the look direction).
pub fn mood(scene: &Scene, look: &CeoLook, my_x: i32, alert_x: impl Fn(&str) -> Option<i32>, t: u64) -> Mood {
    let mut needs: Vec<&office_core::Agent> = scene.agents.values().filter(|a| a.state == AgentState::NeedsYou).collect();
    if !needs.is_empty() {
        needs.sort_by_key(|a| (a.last_event_ms, a.id.clone()));
        let x = alert_x(&needs[0].id).unwrap_or(0);
        return Mood::Alert { left: x <= my_x };
    }
    let busy = scene.agents.values().any(|a| {
        matches!(
            a.state,
            AgentState::Typing | AgentState::Reading | AgentState::Running | AgentState::Thinking | AgentState::Delegating | AgentState::Arriving
        )
    });
    if busy {
        return Mood::Review { nod: (t / 450) % 4 == 1 };
    }
    let cyc = t % 30_000;
    match cyc {
        0..=11_999 => Mood::Coffee { sip: cyc % 4_000 > 2_800 },
        12_000..=23_999 => Mood::LeanBack,
        _ if look.number > 0 => Mood::Back,
        _ => Mood::LeanBack,
    }
}

fn blit_scaled_nearest(dst: &mut RgbaImage, src: &RgbaImage, dx: i32, dy: i32, dw: i32, dh: i32) {
    let (sw, sh) = src.dimensions();
    if dw <= 0 || dh <= 0 || sw == 0 || sh == 0 {
        return;
    }
    let (sw64, sh64, dw64, dh64) = (sw as u64, sh as u64, dw as u64, dh as u64);
    for yy in 0..dh {
        let sy = ((yy as u64 * sh64) / dh64).min(sh64 - 1) as u32;
        for xx in 0..dw {
            let sx = ((xx as u64 * sw64) / dw64).min(sw64 - 1) as u32;
            let p = src.get_pixel(sx, sy).0;
            if p[3] > 0 {
                put(dst, dx + xx, dy + yy, p);
            }
        }
    }
}

/// Draw the CEO with the sprite's top-left at (ox, oy) (unthemed; the caller applies fx).
pub fn draw(img: &mut RgbaImage, ox: i32, oy: i32, l: &CeoLook, m: Mood, blink: bool) {
    draw_with_outfit(img, ox, oy, l, m, blink, None);
}

/// Draw the CEO with an optional external outfit (`Torso` overlay or `Full` nearest-neighbor sprite).
pub fn draw_with_outfit(
    img: &mut RgbaImage,
    ox: i32,
    oy: i32,
    l: &CeoLook,
    m: Mood,
    blink: bool,
    outfit: Option<&CeoOutfit>,
) {
    if let Some(o) = outfit {
        if o.mode == OutfitMode::Full {
            blit_scaled_nearest(img, &o.sprite, ox, oy, W, H);
            return;
        }
    }

    let rect = |img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Col| fill(img, ox + x, oy + y, w, h, c);
    let px = |img: &mut RgbaImage, x: i32, y: i32, c: Col| put(img, ox + x, oy + y, c);
    let skin_d = shade(l.skin, 0.82);
    let lean = if m == Mood::LeanBack { 1 } else { 0 };
    let back = m == Mood::Back;
    let primary = l.primary;
    let secondary = l.secondary;

    // legs: skin knees, black socks, dark boots
    rect(img, 5, 23, 3, 1, l.skin);
    rect(img, 10, 23, 3, 1, l.skin);
    rect(img, 5, 24, 3, 3, BLACK);
    rect(img, 10, 24, 3, 3, BLACK);
    rect(img, 4, 27, 4, 1, rgb(0x2a2420));
    rect(img, 10, 27, 4, 1, rgb(0x2a2420));
    // black shorts
    rect(img, 4, 19, 10, 4, BLACK);
    px(img, 8, 21, shade(BLACK, 1.8));
    px(img, 9, 21, shade(BLACK, 1.8));

    // jersey torso: 2 px vertical stripes in primary/secondary, no crest/logo/sponsor
    let ty = 11 + lean;
    for x in 3..15 {
        let c = if ((x - 3) / 2) % 2 == 0 { primary } else { secondary };
        rect(img, x, ty, 1, 9 - lean, c);
    }
    rect(img, 3, ty + 8 - lean, 12, 1, shade(primary, 0.6));
    if let Some(o) = outfit {
        if o.mode == OutfitMode::Torso {
            blit_scaled_nearest(img, &o.sprite, ox + 3, oy + ty, 12, 9 - lean);
        }
    }
    if back {
        if l.number > 0 {
            let s = l.number.to_string();
            let tw = font::text_width(&s);
            font::draw_text(img, ox + 9 - tw / 2, oy + ty + 2, &s, rgb(0xffffff));
        }
    } else if outfit.is_none() {
        // round collar
        rect(img, 7, ty, 4, 1, secondary);
        px(img, 8, ty, skin_d);
        px(img, 9, ty, skin_d);
    }

    // arms (short striped sleeves, skin forearms)
    let sleeve_l = |img: &mut RgbaImage, y: i32| {
        rect(img, 1, y, 2, 3, primary);
        px(img, 1, y + 2, secondary);
    };
    let sleeve_r = |img: &mut RgbaImage, y: i32| {
        rect(img, 15, y, 2, 3, primary);
        px(img, 16, y + 2, secondary);
    };
    match m {
        Mood::Alert { left } => {
            // point toward the alert with one arm, the other down
            if left {
                rect(img, -3, ty + 1, 5, 2, primary);
                rect(img, -5, ty + 1, 2, 2, l.skin);
                sleeve_r(img, ty);
                rect(img, 15, ty + 3, 2, 4, l.skin);
            } else {
                rect(img, 16, ty + 1, 5, 2, primary);
                rect(img, 21, ty + 1, 2, 2, l.skin);
                sleeve_l(img, ty);
                rect(img, 1, ty + 3, 2, 4, l.skin);
            }
        }
        Mood::Review { .. } => {
            sleeve_l(img, ty);
            sleeve_r(img, ty);
            rect(img, 2, ty + 3, 2, 3, l.skin);
            rect(img, 14, ty + 3, 2, 3, l.skin);
            // papers held in front
            rect(img, 4, ty + 3, 10, 6, rgb(0xf4f1e6));
            rect(img, 5, ty + 4, 7, 1, rgb(0x9aa3ad));
            rect(img, 5, ty + 6, 5, 1, rgb(0x9aa3ad));
            rect(img, 5, ty + 8, 6, 1, primary);
        }
        Mood::Coffee { sip } => {
            sleeve_l(img, ty);
            rect(img, 1, ty + 3, 2, 4, l.skin);
            sleeve_r(img, ty);
            let (mx, my) = if sip { (11, 7) } else { (14, ty + 3) };
            if sip {
                rect(img, 14, ty - 1, 2, 4, l.skin);
            } else {
                rect(img, 15, ty + 3, 2, 3, l.skin);
            }
            // mug
            rect(img, mx, my, 3, 3, rgb(0xf2f2f2));
            px(img, mx + 3, my + 1, rgb(0xf2f2f2));
            rect(img, mx, my, 3, 1, rgb(0x5a3a22));
        }
        Mood::LeanBack => {
            // elbows out, hands behind the head
            rect(img, 0, ty - 4, 2, 5, primary);
            rect(img, 16, ty - 4, 2, 5, primary);
            rect(img, 2, ty - 6 + 1, 2, 2, l.skin);
            rect(img, 14, ty - 6 + 1, 2, 2, l.skin);
        }
        Mood::Back => {
            sleeve_l(img, ty);
            sleeve_r(img, ty);
            rect(img, 1, ty + 3, 2, 4, l.skin);
            rect(img, 15, ty + 3, 2, 4, l.skin);
        }
    }

    // head
    let nod = matches!(m, Mood::Review { nod: true });
    let hdx = match m {
        Mood::Alert { left: true } => -1,
        Mood::Alert { left: false } => 1,
        _ => 0,
    };
    let hx = 4 + hdx;
    let hy = 1 + lean + nod as i32;
    rect(img, 7, hy + 9, 4, 1, skin_d); // neck
    rect(img, hx, hy, 10, 9, l.skin);
    if !back {
        let eye = if blink { skin_d } else { rgb(0x1a1a1a) };
        let ey = hy + 4 + nod as i32;
        let (e1, e2) = (hx + 2 + hdx.min(0), hx + 7 + hdx.max(0));
        px(img, e1, ey, eye);
        px(img, e2, ey, eye);
        rect(img, hx + 4, hy + 7, 2, 1, skin_d); // mouth
        if matches!(m, Mood::Alert { .. }) {
            px(img, hx + 4, hy + 7, rgb(0x5a2a2a)); // "oh!"
        }
        px(img, hx - 1, hy + 4, l.skin); // ears
        px(img, hx + 10, hy + 4, l.skin);
    }
    // hair
    let hc = l.hair;
    let hd = shade(hc, 0.75);
    match l.style {
        HairStyle::Short => {
            rect(img, hx, hy - 1, 10, 3, hc);
            rect(img, hx, hy + 2, 1, 2, hc);
            rect(img, hx + 9, hy + 2, 1, 2, hc);
            rect(img, hx + 2, hy - 1, 6, 1, hd);
        }
        HairStyle::Curly => {
            rect(img, hx - 1, hy - 2, 12, 4, hc);
            rect(img, hx - 1, hy + 2, 2, 3, hc);
            rect(img, hx + 9, hy + 2, 2, 3, hc);
            for k in 0..6 {
                px(img, hx - 1 + k * 2, hy - 3 + (k % 2), hc);
                px(img, hx + k * 2, hy - 1, hd);
            }
        }
        HairStyle::Long => {
            rect(img, hx - 1, hy - 1, 12, 3, hc);
            rect(img, hx - 1, hy + 2, 2, 10, hc);
            rect(img, hx + 9, hy + 2, 2, 10, hc);
            rect(img, hx + 3, hy - 1, 4, 1, hd);
        }
        HairStyle::Bald => {
            px(img, hx + 3, hy, shade(l.skin, 1.15));
            px(img, hx + 4, hy, shade(l.skin, 1.15));
            rect(img, hx, hy + 3, 1, 2, hc);
            rect(img, hx + 9, hy + 3, 1, 2, hc);
        }
    }
    if back {
        // back of the head
        match l.style {
            HairStyle::Bald => {}
            HairStyle::Long => rect(img, hx - 1, hy, 12, 9, hc),
            _ => rect(img, hx, hy, 10, 6, hc),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::demo;
    use super::*;

    #[test]
    fn mood_mirrors_the_office() {
        let now = 10_000_000;
        let l = look(&CeoConfig::default());
        let s = demo::demo_scene(now);
        assert!(matches!(mood(&s, &l, 500, |_| Some(10), now), Mood::Alert { left: true }));
        let mut busy = s.clone();
        for a in busy.agents.values_mut() {
            if a.state == AgentState::NeedsYou {
                a.state = AgentState::Typing;
            }
        }
        assert!(matches!(mood(&busy, &l, 500, |_| None, now), Mood::Review { .. }));
        let idle = Scene::default();
        let moods: Vec<Mood> = (0..30).map(|i| mood(&idle, &l, 0, |_| None, i * 1_000)).collect();
        assert!(moods.iter().any(|m| matches!(m, Mood::Coffee { sip: true })));
        assert!(moods.contains(&Mood::LeanBack) && moods.contains(&Mood::Back));
    }

    #[test]
    fn looks_parse_and_draw() {
        for style in ["short", "curly", "long", "bald", "?"] {
            let c = CeoConfig { hair_style: style.into(), hair: "#336699".into(), ..Default::default() };
            let l = look(&c);
            assert_eq!(l.hair, rgb(0x336699));
            for m in [Mood::Alert { left: true }, Mood::Alert { left: false }, Mood::Review { nod: true }, Mood::Coffee { sip: true }, Mood::LeanBack, Mood::Back] {
                let mut img = RgbaImage::new(W as u32 + 12, H as u32 + 4);
                draw(&mut img, 6, 2, &l, m, false);
                assert!(img.pixels().any(|p| p.0 == RED));
            }
        }
        assert_eq!(look(&CeoConfig::default()).skin, rgb(0xc68642));
    }

    #[test]
    fn load_outfit_validates_and_renders_modes_and_colors() {
        let root = std::env::temp_dir().join(format!("office-ceo-outfit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pack = root.join("tuxedo");
        std::fs::create_dir_all(&pack).unwrap();

        let mut spr = RgbaImage::new(12, 9);
        for p in spr.pixels_mut() {
            p.0 = [0x22, 0x88, 0xee, 255];
        }
        spr.save(pack.join("outfit.png")).unwrap();
        std::fs::write(pack.join("outfit.toml"), "name = \"tuxedo\"\nmode = \"torso\"\nsprite = \"outfit.png\"\n").unwrap();

        // None / empty outfit -> Ok(None)
        assert!(load_outfit(&CeoConfig::default(), &root).unwrap().is_none());
        let empty = CeoConfig { outfit: Some("  ".into()), ..Default::default() };
        assert!(load_outfit(&empty, &root).unwrap().is_none());

        // Path traversal rejected
        let bad = CeoConfig { outfit: Some("../bad".into()), ..Default::default() };
        assert!(load_outfit(&bad, &root).is_err());

        // Valid Torso load + render
        let cfg_torso = CeoConfig {
            outfit: Some("tuxedo".into()),
            outfit_mode: "torso".into(),
            primary_color: Some("#11aa22".into()),
            secondary_color: Some("#334455".into()),
            ..Default::default()
        };
        let outfit_torso = load_outfit(&cfg_torso, &root).unwrap().expect("outfit loaded");
        assert_eq!(outfit_torso.mode, OutfitMode::Torso);
        let l = look(&cfg_torso);
        assert_eq!(l.primary, rgb(0x11aa22));
        assert_eq!(l.secondary, rgb(0x334455));

        let mut img_custom = RgbaImage::new(W as u32 + 12, H as u32 + 4);
        draw_with_outfit(&mut img_custom, 6, 2, &l, Mood::Coffee { sip: false }, false, None);
        assert!(img_custom.pixels().any(|p| p.0 == rgb(0x11aa22)));
        assert!(img_custom.pixels().any(|p| p.0 == rgb(0x334455)));

        let mut img_torso = RgbaImage::new(W as u32 + 12, H as u32 + 4);
        draw_with_outfit(&mut img_torso, 6, 2, &l, Mood::Coffee { sip: false }, false, Some(&outfit_torso));
        assert!(img_torso.pixels().any(|p| p.0 == [0x22, 0x88, 0xee, 255]));
        // Procedural head still drawn in Torso mode
        assert!(img_torso.pixels().any(|p| p.0 == l.skin));

        // Valid Full load + render
        let cfg_full = CeoConfig {
            outfit: Some("tuxedo".into()),
            outfit_mode: "full".into(),
            ..Default::default()
        };
        let outfit_full = load_outfit(&cfg_full, &root).unwrap().expect("full outfit loaded");
        assert_eq!(outfit_full.mode, OutfitMode::Full);
        let mut img_full = RgbaImage::new(W as u32 + 12, H as u32 + 4);
        draw_with_outfit(&mut img_full, 6, 2, &l, Mood::LeanBack, false, Some(&outfit_full));
        assert!(img_full.pixels().any(|p| p.0 == [0x22, 0x88, 0xee, 255]));
        assert!(!img_full.pixels().any(|p| p.0 == l.skin));

        let _ = std::fs::remove_dir_all(&root);
    }
}
