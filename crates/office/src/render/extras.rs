//! ROSSO system extras: verdict chips, the Security Desk guard, document icons, in-tray badges,
//! the council wall-screen step dots and the entrypoint tag. Pure drawing helpers; `th.fx`
//! recolours everything for the active theme.
use super::canvas::{fill, outline, put, rgb, with_alpha, Col};
use super::character::{self, Arms, Legs, Pose};
use super::font;
use super::theme::Theme;
use image::RgbaImage;
use office_core::rosso::{auto_phase_progress, verdict_class};
use office_core::{AutoJob, DispatchMode, QueueTicket, WaveState};

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
    let on = (t / 350).is_multiple_of(2);
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
    let arms = if alert { Arms::Wave((t / 300).is_multiple_of(2)) } else { Arms::Down };
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
    let on = (t / 300).is_multiple_of(2);
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
        if i == current && (t / 400).is_multiple_of(2) {
            fill(img, x, sy + 7, 2, 1, f(0xffffff));
        }
    }
    let n = reached.iter().filter(|r| **r).count() as i32;
    fill(img, sx + 28, sy + 9, 11, 1, f(0x24406a));
    fill(img, sx + 28, sy + 9, (n * 11 / 4).max(if n > 0 { 1 } else { 0 }), 1, f(0x5fd35f));
}

/// Compact FIFO queue chip label: `#<pos> BUSY` when repo-blocked, else `#<pos>`.
pub fn queue_chip_text(t: &QueueTicket) -> String {
    if t.repo_busy {
        format!("#{} BUSY", t.pos)
    } else {
        format!("#{}", t.pos)
    }
}

/// Pixel width a queue chip adds after a name tag (1 px gap + text + padding).
pub fn queue_chip_width(t: &QueueTicket) -> i32 {
    font::text_width(&queue_chip_text(t)) + 4
}

/// Draw a FIFO queue chip whose left edge (incl. the 1 px gap) is `x`, in a name tag of height `h`.
pub fn draw_queue_chip(img: &mut RgbaImage, th: &Theme, x: i32, y: i32, h: i32, t: &QueueTicket) {
    let txt = queue_chip_text(t);
    let bg = if t.repo_busy {
        rgb(0xe67e22)
    } else if t.pos == 1 {
        rgb(0xffd23f)
    } else {
        rgb(0xf2b134)
    };
    let w = font::text_width(&txt) + 3;
    fill(img, x + 1, y, w, h, th.fx.apply(bg));
    font::draw_text(img, x + 3, y + 1, &txt, th.fx.apply(rgb(0x1b1d22)));
}

fn wave_call_color(c: &office_core::WaveCall, t: u64) -> Col {
    if c.relay {
        if (t / 350).is_multiple_of(2) {
            rgb(0x4dd0e1)
        } else {
            rgb(0x2b7a8c)
        }
    } else if c.status.eq_ignore_ascii_case("OK") {
        match c.verdict.as_deref() {
            None => rgb(0x3fae6a),
            Some(v) => match verdict_class(v) {
                0 => rgb(0x3fae6a),
                2 => rgb(0xe03c3c),
                _ => rgb(0xf2b134),
            },
        }
    } else {
        rgb(0xe03c3c)
    }
}

/// Wave progress strip in the back-wall decor strip, centered horizontally around `cx` at top `y`
/// (clamped `>= 2`). Returns its `[x, y, w, 11]` rect for overlay collision avoidance.
pub fn draw_wave_strip(img: &mut RgbaImage, th: &Theme, cx: i32, y: i32, wave: &WaveState, t: u64) -> [i32; 4] {
    let total = (wave.done + wave.relay_inflight + wave.pending).max(wave.done + wave.relay_inflight);
    let mut txt = format!("WAVE {}/{}", wave.done, total);
    if wave.relay_inflight > 0 {
        txt.push_str(&format!(" +{} RLY", wave.relay_inflight));
    }
    let shown_calls = wave.calls.len().min(8);
    let shown_pending = (wave.pending as usize).min(8 - shown_calls);
    let n_blocks = shown_calls + shown_pending;
    let tw = font::text_width(&txt);
    let blocks_w = if n_blocks > 0 { n_blocks as i32 * 4 - 1 } else { 0 };
    let gap = if n_blocks > 0 { 4 } else { 0 };
    let w = 4 + tw + gap + blocks_w + 4;
    let h = 11;
    let y = y.max(2);
    let max_x = (img.width() as i32 - w - 2).max(2);
    let x = (cx - w / 2).clamp(2, max_x);
    let f = |c: Col| th.fx.apply(c);
    fill(img, x, y, w, h, f(rgb(0x181c24)));
    outline(img, x, y, w, h, f(rgb(0x3a4354)));
    font::draw_text(img, x + 4, y + 3, &txt, f(rgb(0xfbfaf5)));
    let mut bx = x + 4 + tw + gap;
    for c in wave.calls.iter().take(8) {
        fill(img, bx, y + 3, 3, 5, f(wave_call_color(c, t)));
        bx += 4;
    }
    for _ in 0..shown_pending {
        fill(img, bx, y + 3, 3, 5, f(rgb(0x333c4d)));
        bx += 4;
    }
    [x, y, w, h]
}

fn lerp_col(a: Col, b: Col, t: f32) -> Col {
    let k = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round().clamp(0.0, 255.0) as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]), 255]
}

fn auto_status_colors(status: &str, t: f32) -> (Col, Col) {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => {
            let pulse = (t * 0.713 * std::f32::consts::TAU).cos() * 0.5 + 0.5;
            (rgb(0x3fae6a), lerp_col(rgb(0x1f6b43), rgb(0x52f296), pulse))
        }
        "needs_user_input" => (rgb(0xf2b134), rgb(0xffd23f)),
        "done" => (rgb(0x4aa3df), rgb(0x7cc4f5)),
        _ => (rgb(0xe03c3c), rgb(0xff6b6b)),
    }
}

fn auto_phase_label(phase: &str) -> String {
    let cleaned: String = phase
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_uppercase()
            } else {
                ' '
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "INIT".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `rosso-auto` back-wall status pill centered horizontally at `cx` with top `y` (clamped inside
/// image bounds). Returns its `[x, y, w, h]` rect for overlay collision avoidance.
pub fn draw_auto_strip(img: &mut RgbaImage, th: &Theme, cx: i32, y: i32, aj: &AutoJob, t: f32) -> [i32; 4] {
    let h = 12;
    let max_y = (img.height() as i32 - h - 2).max(2);
    let y = y.clamp(2, max_y);
    let max_w = (img.width() as i32 - 4).max(16);
    let (step, total) = auto_phase_progress(&aj.flow, &aj.phase);
    let min_bar_w = if total > 0 { (6 + total as i32 * 4).min(max_w) } else { 24.min(max_w) };

    let phase = auto_phase_label(&aj.phase);
    let raw_txt = format!("AUTO {} {}/{}", phase, aj.calls_used, aj.max_calls);
    let txt = font::truncate(&raw_txt, (max_w - 12).max(0));
    let tw = font::text_width(&txt);
    let w = (12 + tw).max(min_bar_w).min(max_w);
    let max_x = (img.width() as i32 - w - 2).max(2);
    let x = (cx - w / 2).clamp(2, max_x);

    let f = |c: Col| th.fx.apply(c);
    let (border, dot) = auto_status_colors(&aj.status, t);
    let is_done = aj.status.trim().eq_ignore_ascii_case("done");

    fill(img, x, y, w, h, f(rgb(0x181c24)));
    outline(img, x, y, w, h, f(border));
    fill(img, x + 3, y + 3, 3, 3, f(dot));
    font::draw_text(img, x + 8, y + 2, &txt, f(rgb(0xfbfaf5)));

    let bar_x = x + 3;
    let bar_w = (w - 6).max(1);
    if total > 0 {
        for i in 0..total {
            let sx0 = bar_x + (i as i32 * (bar_w + 1)) / total as i32;
            let sx1 = bar_x + ((i as i32 + 1) * (bar_w + 1)) / total as i32 - 1;
            let sw = (sx1 - sx0).max(1);
            let idx = i + 1;
            let col = if idx < step || (idx == step && is_done) {
                border
            } else if idx == step {
                dot
            } else {
                rgb(0x2c3444)
            };
            fill(img, sx0, y + 8, sw, 2, f(col));
        }
    } else {
        fill(img, bar_x, y + 8, bar_w, 2, f(rgb(0x2c3444)));
        if aj.max_calls > 0 && aj.calls_used > 0 {
            let filled = ((aj.calls_used as i64 * bar_w as i64) / aj.max_calls as i64).clamp(1, bar_w as i64) as i32;
            fill(img, bar_x, y + 8, filled, 2, f(border));
        }
    }

    [x, y, w, h]
}

/// Dispatch route tag (`DIRECT`, `RELAY`, `DIR+RLY`) centered at `cx` with top `y`.
pub fn draw_dispatch_tag(img: &mut RgbaImage, th: &Theme, cx: i32, y: i32, mode: DispatchMode) {
    let (txt, bg) = match mode {
        DispatchMode::Direct => ("DIRECT", rgb(0x2dd4bf)),
        DispatchMode::Relay => ("RELAY", rgb(0xc084fc)),
        DispatchMode::Hybrid => ("DIR+RLY", rgb(0xf59e0b)),
    };
    let w = font::text_width(txt) + 4;
    let h = font::GLYPH_H + 2;
    let x = cx - w / 2;
    fill(img, x, y, w, h, th.fx.apply(bg));
    font::draw_text(img, x + 2, y + 1, txt, th.fx.apply(rgb(0x14171c)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_chip_text_formats_position_and_busy() {
        let q1 = QueueTicket { pos: 1, since_ms: 100, repo_busy: false };
        let q2 = QueueTicket { pos: 2, since_ms: 200, repo_busy: true };
        assert_eq!(queue_chip_text(&q1), "#1");
        assert_eq!(queue_chip_text(&q2), "#2 BUSY");
        assert!(queue_chip_width(&q2) > queue_chip_width(&q1));
    }

    #[test]
    fn wave_strip_draws_pill_and_clamps_y() {
        let mut img = RgbaImage::new(240, 40);
        let th = super::super::theme::procedural();
        let wave = WaveState {
            job: "j1".into(),
            plan: "council-r2".into(),
            updated_ms: 1_000,
            done: 2,
            relay_inflight: 1,
            pending: 1,
            calls: vec![
                office_core::WaveCall { title: "Reliability Seat".into(), status: "OK".into(), verdict: Some("SUPPORT".into()), relay: false },
                office_core::WaveCall { title: "Security Seat".into(), status: "OK".into(), verdict: Some("REVISE".into()), relay: false },
                office_core::WaveCall { title: "Chief Inspector".into(), status: "RUNNING".into(), verdict: None, relay: true },
            ],
        };
        let rect = draw_wave_strip(&mut img, &th, 120, 0, &wave, 0);
        assert_eq!(rect[1], 2);
        assert_eq!(rect[3], 11);
        assert!(rect[2] > 60);
        assert_eq!(img.get_pixel(rect[0] as u32, rect[1] as u32).0, rgb(0x3a4354));
        assert_eq!(img.get_pixel((rect[0] + 1) as u32, (rect[1] + 1) as u32).0, rgb(0x181c24));
        // 4 blocks (3 calls + 1 pending) at the right end of the pill
        let b0 = rect[0] + rect[2] - 4 - (4 * 4 - 1);
        assert_eq!(img.get_pixel(b0 as u32, (rect[1] + 3) as u32).0, rgb(0x3fae6a));
        assert_eq!(img.get_pixel((b0 + 4) as u32, (rect[1] + 3) as u32).0, rgb(0xf2b134));
        assert_eq!(img.get_pixel((b0 + 8) as u32, (rect[1] + 3) as u32).0, rgb(0x4dd0e1));
        assert_eq!(img.get_pixel((b0 + 12) as u32, (rect[1] + 3) as u32).0, rgb(0x333c4d));
    }

    #[test]
    fn dispatch_tag_draws_expected_colors() {
        let mut img = RgbaImage::new(80, 20);
        let th = super::super::theme::procedural();
        draw_dispatch_tag(&mut img, &th, 40, 2, DispatchMode::Direct);
        let w = font::text_width("DIRECT") + 4;
        assert_eq!(img.get_pixel((40 - w / 2) as u32, 2).0, rgb(0x2dd4bf));

        draw_dispatch_tag(&mut img, &th, 40, 10, DispatchMode::Hybrid);
        let hw = font::text_width("DIR+RLY") + 4;
        assert_eq!(img.get_pixel((40 - hw / 2) as u32, 10).0, rgb(0xf59e0b));
    }

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

    #[test]
    fn auto_strip_draws_pill_and_clamps_y() {
        let mut img = RgbaImage::new(240, 40);
        let th = super::super::theme::procedural();
        let aj = AutoJob {
            job: "auto-1".into(),
            flow: "council".into(),
            phase: "critique".into(),
            status: "running".into(),
            calls_used: 4,
            max_calls: 12,
            ..Default::default()
        };

        let rect = draw_auto_strip(&mut img, &th, 120, 0, &aj, 0.0);
        assert_eq!(rect[1], 2);
        assert_eq!(rect[3], 12);
        assert!(rect[2] > 60);
        assert_eq!(img.get_pixel(rect[0] as u32, rect[1] as u32).0, rgb(0x3fae6a));
        assert_eq!(img.get_pixel((rect[0] + 1) as u32, (rect[1] + 1) as u32).0, rgb(0x181c24));
        // segmented bar: first segment completed (green), last segment pending (dark slate)
        assert_eq!(img.get_pixel((rect[0] + 3) as u32, (rect[1] + 8) as u32).0, rgb(0x3fae6a));
        assert_eq!(img.get_pixel((rect[0] + rect[2] - 4) as u32, (rect[1] + 8) as u32).0, rgb(0x2c3444));

        // pulsing dot changes with time t while status is running
        let dot_t0 = img.get_pixel((rect[0] + 3) as u32, (rect[1] + 3) as u32).0;
        let mut img_t1 = RgbaImage::new(240, 40);
        draw_auto_strip(&mut img_t1, &th, 120, 0, &aj, 0.5);
        let dot_t1 = img_t1.get_pixel((rect[0] + 3) as u32, (rect[1] + 3) as u32).0;
        assert_ne!(dot_t0, dot_t1);

        // bottom y clamping inside image bounds
        let mut img_bot = RgbaImage::new(240, 40);
        let bot_rect = draw_auto_strip(&mut img_bot, &th, 120, 100, &aj, 0.0);
        assert_eq!(bot_rect[1], 40 - 12 - 2);
        assert!(bot_rect[1] + bot_rect[3] <= 40);

        // status border color coding for warn / done / error
        for (status, expected) in [
            ("needs_user_input", rgb(0xf2b134)),
            ("done", rgb(0x4aa3df)),
            ("timeout", rgb(0xe03c3c)),
        ] {
            let mut s_img = RgbaImage::new(240, 40);
            let mut s_aj = aj.clone();
            s_aj.status = status.into();
            let r = draw_auto_strip(&mut s_img, &th, 120, 4, &s_aj, 0.0);
            assert_eq!(s_img.get_pixel(r[0] as u32, r[1] as u32).0, expected, "status {status}");
        }
    }
}
