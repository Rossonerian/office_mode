//! ROSSO system extras: verdict chips, the Security Desk guard, document icons, in-tray badges,
//! the council wall-screen step dots and the entrypoint tag. Pure drawing helpers; `th.fx`
//! recolours everything for the active theme.
use super::canvas::{fill, outline, put, rgb, with_alpha, Col};
use super::character::{self, Arms, Legs, Pose};
use super::font;
use super::theme::Theme;
use image::RgbaImage;
use office_core::rosso::verdict_class;

/// How long the guard keeps his hand up after a denial.
pub const GUARD_ALERT_MS: u64 = 6_000;

/// Short text of a verdict for its chip.
pub fn chip_text(v: &str) -> &'static str {
    match v {
        "APPROVE" => "APPR",
        "CHANGES_REQUESTED" => "CHG",
        "APPROVE_WITH_CONDITIONS" => "COND",
        "PASS" => "PASS",
        "FAIL" => "FAIL",
        "INSUFFICIENT" => "INSF",
        "SUPPORT" => "SUPP",
        "REVISE" => "REV",
        "OBJECT" => "OBJ",
        _ => "?",
    }
}

/// Pixel width a chip adds after a name tag (1 px gap + text + padding).
pub fn chip_width(v: &str) -> i32 {
    font::text_width(chip_text(v)) + 4
}

/// Does this verdict pulse? (FAIL, OBJECT: red; REVISE: amber.)
pub fn chip_pulses(v: &str) -> bool {
    matches!(v, "FAIL" | "OBJECT" | "REVISE")
}

/// Chip background and text colour for a verdict at time `t`.
pub fn chip_colors(v: &str, t: u64) -> (Col, Col) {
    let on = (t / 350) % 2 == 0;
    match verdict_class(v) {
        0 => (rgb(0x3fae6a), rgb(0x0e2418)),
        2 => (if on { rgb(0xe03c3c) } else { rgb(0x8f2323) }, rgb(0xfff4f4)),
        _ if chip_pulses(v) && !on => (rgb(0xb8861a), rgb(0x1b1d22)),
        _ => (rgb(0xf2b134), rgb(0x1b1d22)),
    }
}

/// Draw a verdict chip whose left edge (incl. the gap) is `x`, in a name tag of height `h`.
pub fn draw_chip(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, h: i32, v: &str, t: u64) {
    let (bg, fg) = chip_colors(v, t);
    let w = chip_width(v) - 1;
    fill(img, x + 1, y, w, h, th.fx.apply(bg));
    font::draw_text(img, x + 3, y + 1, chip_text(v), th.fx.apply(fg));
}

/// Compact tag text for a ROSSO title, shortest last; the full label comes first. `label` may carry
/// a "·model" suffix, which the short forms drop.
fn title_candidates(label: &str) -> Vec<String> {
    let base = label.split('\u{b7}').next().unwrap_or(label).trim();
    let lower = base.to_ascii_lowercase();
    let seat = |x: &str| match x {
        "security" => "SEC",
        "correctness" => "COR",
        "reliability" => "REL",
        "performance" => "PERF",
        "maintainability" => "MNT",
        "client intent" => "INT",
        _ => "",
    };
    let mut out: Vec<String> = vec![label.to_string()];
    let mut add = |v: &[&str]| out.extend(v.iter().map(|x| x.to_string()));
    match lower.as_str() {
        "chief architect" => add(&["CHIEF ARCH", "ARCHITECT", "ARCH"]),
        "chief inspector" => add(&["INSPECTOR", "INSP"]),
        "test inspector" => add(&["TESTER", "TEST"]),
        "design engineer" => add(&["DESIGNER", "DESIGN"]),
        "technician" => add(&["TECH"]),
        "records clerk" => add(&["CLERK"]),
        "mail clerk" => add(&["MAIL CLERK", "CLERK"]),
        _ => {
            if let Some(x) = lower.strip_suffix(" seat") {
                let short = if seat(x).is_empty() { x.chars().take(3).collect::<String>().to_ascii_uppercase() } else { seat(x).to_string() };
                add(&[&format!("{short} SEAT"), &short]);
            } else if let Some(d) = lower.strip_suffix(" head") {
                let short = match d {
                    "frontend" => "FRONT",
                    "backend" => "BACK",
                    "database" => "DB",
                    "integration" => "INTEG",
                    "performance" => "PERF",
                    "research" => "RES",
                    "security" => "SEC",
                    other => other,
                };
                add(&[&format!("{} HEAD", d.to_ascii_uppercase()), &format!("{} HEAD", short.to_ascii_uppercase()), &short.to_ascii_uppercase()]);
            }
        }
    }
    out
}

/// The name-tag text for `label` within `max_px`: the full label when it fits (always on hover or
/// selection, where the budget is large), else the first ROSSO abbreviation that fits, else a
/// plain truncation.
pub fn tag_name(label: &str, max_px: i32) -> String {
    let cands = title_candidates(label);
    if let Some(c) = cands.iter().find(|c| font::text_width(c) <= max_px) {
        return c.clone();
    }
    // nothing fits: truncate the shortest known form (or the label itself when it has none)
    font::truncate(cands.last().map(String::as_str).unwrap_or(label), max_px)
}

/// Compact failure hint for its bubble: "⟳ retry", "✂ split", "✕ see log".
pub fn compact_hint(hint: &str) -> String {
    let h = hint.to_ascii_lowercase();
    if h.contains("retry") {
        "⟳ retry".into()
    } else if h.contains("split") {
        "✂ split".into()
    } else if h.contains("log") {
        "✕ see log".into()
    } else {
        font::truncate_words(hint, 44)
    }
}

/// "DESKTOP" / "CLI" for a session entrypoint.
pub fn entry_tag(entrypoint: &str) -> &'static str {
    if entrypoint.contains("desktop") {
        "DESKTOP"
    } else if entrypoint == "cli" {
        "CLI"
    } else {
        ""
    }
}

pub fn draw_entry_tag(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, tag: &str) -> i32 {
    let w = font::text_width(tag) + 3;
    let bg = if tag == "CLI" { rgb(0x52b788) } else { rgb(0x7f8fe6) };
    fill(img, x, y, w, font::GLYPH_H + 2, th.fx.apply(bg));
    font::draw_text(img, x + 2, y + 1, tag, th.fx.apply(rgb(0x14171c)));
    w
}

/// The guard's name tag text.
pub const GUARD_TAG: &str = "SECURITY";

/// Size of the raised-hand icon bubble including its tail.
pub const HAND_BUBBLE: (i32, i32) = (9, 11);

/// The Security Desk guard: dark uniform, cap, gold badge. Feet at (fx, fy). With `alert` he
/// raises a hand. His name tag and the red hand bubble are overlays placed by the collision
/// resolver (`guard_tag_candidates` / `guard_icon_candidates`).
pub fn draw_guard(img: &mut RgbaImage, th: &Theme, fx: i32, fy: i32, alert: bool, t: u64) {
    let mut look = character::look_for("security-desk");
    look.hair = rgb(0x151a26); // cap
    look.hair_style = 0;
    look.shirt = rgb(0x20262f);
    look.pants = rgb(0x171b22);
    look.glasses = false;
    let arms = if alert { Arms::Wave((t / 300) % 2 == 0) } else { Arms::Down };
    let pose = Pose { arms, legs: Legs::Stand, ..Default::default() };
    let f = |c: u32| th.fx.apply(rgb(c));
    fill(img, fx - 5, fy - 1, 10, 2, with_alpha(f(0x000000), 60));
    let mut buf = RgbaImage::new(character::CW as u32 + 4, character::CH as u32 + 8);
    character::draw(&mut buf, 2, 6, &look, &pose);
    super::themed_sprite(&mut buf, th, false);
    super::canvas::blit(img, &buf, fx - 10, fy - 30);
    // cap brim and peak, gold badge, shoulder stripe
    fill(img, fx - 5, fy - 23, 11, 1, f(0x0c0f16));
    put(img, fx - 1, fy - 25, f(0xe8c547));
    put(img, fx + 1, fy - 13, f(0xe8c547));
    put(img, fx + 1, fy - 12, f(0xe8c547));
    fill(img, fx - 5, fy - 14, 1, 3, f(0x3a4352));
}

/// Candidate rects `[x, y, w, h]` for the guard's "SECURITY" tag, best first: above the cap,
/// beside the head, then below the feet. (`fx`,`fy` = his feet.)
pub fn guard_tag_candidates(fx: i32, fy: i32) -> Vec<[i32; 4]> {
    let w = font::text_width(GUARD_TAG) + 2;
    let h = font::GLYPH_H + 2;
    vec![
        [fx - w / 2, fy - 38, w, h],
        [fx + 7, fy - 28, w, h],
        [fx - 7 - w, fy - 28, w, h],
        [fx - w / 2, fy + 3, w, h],
    ]
}

/// Candidate rects for the hand bubble: above the tag first, then beside it.
pub fn guard_icon_candidates(fx: i32, fy: i32) -> Vec<[i32; 4]> {
    let (w, h) = HAND_BUBBLE;
    vec![[fx - w / 2, fy - 52, w, h], [fx + 19, fy - 42, w, h], [fx - 19 - w, fy - 42, w, h], [fx + 19, fy - 54, w, h]]
}

/// Draw the guard's name tag in its resolved rect.
pub fn draw_guard_tag(img: &mut RgbaImage, th: &Theme, rect: [i32; 4]) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let w = font::text_width(GUARD_TAG);
    font::label(img, rect[0] + 1 + w / 2, rect[1] + 1, GUARD_TAG, f(0xf2f4f7), f(0x7a1f1f));
}

/// Red bubble with a raised-hand glyph (the "stop" hand).
pub fn draw_hand_bubble(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, t: u64) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let on = (t / 300) % 2 == 0;
    fill(img, x, y, 9, 9, if on { f(0xe03c3c) } else { f(0xb02828) });
    outline(img, x, y, 9, 9, f(0x1b1d22));
    put(img, x + 1, y + 9, f(0x1b1d22));
    put(img, x + 2, y + 10, f(0x1b1d22));
    let w = f(0xfff4f4);
    // four fingers, palm, thumb
    for fx_ in [2, 3, 4, 5] {
        fill(img, x + fx_, y + 2, 1, 3, w);
    }
    fill(img, x + 2, y + 4, 4, 3, w);
    put(img, x + 6, y + 5, w);
    put(img, x + 1, y + 5, w);
}

/// A small stack of paper with its count: a ROSSO in-tray. Top-left of the stack at (x, y).
pub fn draw_in_tray(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, count: u32) {
    let f = |c: u32| th.fx.apply(rgb(c));
    let layers = count.clamp(1, 3) as i32;
    fill(img, x - 1, y + 3, 9, 1, f(0x8a6a3a)); // tray
    for k in 0..layers {
        let yy = y + 2 - k;
        fill(img, x, yy, 7, 1, f(if k % 2 == 0 { 0xf6f3ea } else { 0xdcd8c8 }));
    }
    put(img, x + 1, y + 2 - layers + 1, f(0x8a8f9c));
    let txt = if count > 99 { "99".to_string() } else { count.to_string() };
    let tw = font::text_width(&txt);
    fill(img, x + 8, y - 3, tw + 2, font::GLYPH_H + 1, f(0xd23c3c));
    font::draw_text(img, x + 9, y - 2, &txt, f(0xffffff));
}

const STEP_COLORS: [u32; 4] = [0x4aa3df, 0xe05252, 0xf29a3a, 0x4cc46a];

/// The four council round steps (Task Card, Critique, Rebuttal, Decision Record) on the meeting
/// room's wall screen: reached steps lit, the current one underlined. `sx`,`sy` = the screen's
/// top-left (the cabin's AGENDA screen).
pub fn draw_council_steps(img: &mut RgbaImage, th: &Theme, sx: i32, sy: i32, reached: [bool; 4], current: usize, t: u64) {
    let f = |c: u32| th.fx.apply(rgb(c));
    fill(img, sx + 26, sy + 4, 15, 6, f(0x16263d));
    for i in 0..4 {
        let x = sx + 28 + i as i32 * 3;
        let c = if reached[i] { STEP_COLORS[i] } else { 0x2c3e57 };
        fill(img, x, sy + 5, 2, 2, f(c));
        if i == current && (t / 400) % 2 == 0 {
            fill(img, x, sy + 7, 2, 1, f(0xffffff));
        }
    }
    let n = reached.iter().filter(|r| **r).count() as i32;
    fill(img, sx + 28, sy + 9, 11, 1, f(0x24406a));
    fill(img, sx + 28, sy + 9, (n * 11 / 4).max(if n > 0 { 1 } else { 0 }), 1, f(0x5fd35f));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_abbreviate_only_when_they_must() {
        assert_eq!(tag_name("Security Seat", 200), "Security Seat");
        assert_eq!(tag_name("Security Seat", 33), "SEC SEAT");
        assert_eq!(tag_name("Correctness Seat", 33), "COR SEAT");
        assert_eq!(tag_name("Reliability Seat", 33), "REL SEAT");
        assert_eq!(tag_name("Chief Architect", 40), "CHIEF ARCH");
        assert_eq!(tag_name("Chief Inspector", 36), "INSPECTOR");
        assert_eq!(tag_name("Chief Inspector\u{b7}neon-medium", 36), "INSPECTOR");
        assert_eq!(tag_name("Test Inspector", 28), "TESTER");
        assert_eq!(tag_name("Frontend Head", 52), "Frontend Head");
        assert_eq!(tag_name("Frontend Head", 48), "FRONT HEAD");
        assert_eq!(tag_name("Frontend Head", 43), "FRONT HEAD");
        assert_eq!(tag_name("Frontend Head", 25), "FRONT");
        // unknown labels are plainly truncated
        assert!(tag_name("worker\u{b7}hydrogen-low", 30).ends_with('\u{2026}'));
    }

    #[test]
    fn failure_hints_are_compact() {
        assert_eq!(compact_hint("stalled: retry once"), "⟳ retry");
        assert_eq!(compact_hint("too big: split the task"), "✂ split");
        assert!(font::text_width(&compact_hint("failed: see the run log")) <= 44);
    }

    #[test]
    fn overlays_need_a_real_two_pixel_gap() {
        use super::super::rects_overlap as o;
        assert!(o([0, 0, 10, 9], [10, 0, 10, 9]), "touching");
        assert!(o([0, 0, 10, 9], [11, 0, 10, 9]), "1 px apart is still too close");
        assert!(!o([0, 0, 10, 9], [12, 0, 10, 9]), "2 px apart is clear");
        assert!(o([0, 0, 10, 9], [0, 10, 10, 9]), "1 px apart vertically");
        assert!(!o([0, 0, 10, 9], [0, 11, 10, 9]));
    }

    #[test]
    fn guard_tag_and_icon_never_overlap_by_default() {
        let tag = guard_tag_candidates(100, 100)[0];
        let icon = guard_icon_candidates(100, 100)[0];
        assert!(!super::super::rects_overlap(tag, icon));
    }
}
