//! The CEO (you): an original procedural sprite (18×28 at 1×, a bit larger than the agents) in a
//! football jersey with vertical red and black stripes (no crest, logo or sponsor), black shorts
//! and socks, and an optional number on the back. Its mood mirrors the office: it turns toward a
//! NeedsYou alert, reviews papers and nods while agents work, and sips coffee or leans back when
//! everything is idle.
use super::canvas::{fill, parse_hex, put, rgb, shade, Col};
use super::font;
use image::RgbaImage;
use office_core::{config::CeoConfig, AgentState, Scene};

pub const W: i32 = 18;
pub const H: i32 = 28;

const RED: Col = rgb(0xd0202a);
const BLACK: Col = rgb(0x18181b);

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
    CeoLook { skin: parse_hex(&c.skin).unwrap_or(rgb(0xc68642)), hair, style, number: c.number.min(99) }
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

/// Draw the CEO with the sprite's top-left at (ox, oy) (unthemed; the caller applies fx).
pub fn draw(img: &mut RgbaImage, ox: i32, oy: i32, l: &CeoLook, m: Mood, blink: bool) {
    let rect = |img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Col| fill(img, ox + x, oy + y, w, h, c);
    let px = |img: &mut RgbaImage, x: i32, y: i32, c: Col| put(img, ox + x, oy + y, c);
    let skin_d = shade(l.skin, 0.82);
    let lean = if m == Mood::LeanBack { 1 } else { 0 };
    let back = m == Mood::Back;

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

    // jersey torso: 2 px vertical red/black stripes, no crest/logo/sponsor
    let ty = 11 + lean;
    for x in 3..15 {
        let c = if ((x - 3) / 2) % 2 == 0 { RED } else { BLACK };
        rect(img, x, ty, 1, 9 - lean, c);
    }
    rect(img, 3, ty + 8 - lean, 12, 1, shade(RED, 0.6));
    if back {
        if l.number > 0 {
            let s = l.number.to_string();
            let tw = font::text_width(&s);
            font::draw_text(img, ox + 9 - tw / 2, oy + ty + 2, &s, rgb(0xffffff));
        }
    } else {
        // round collar
        rect(img, 7, ty, 4, 1, BLACK);
        px(img, 8, ty, skin_d);
        px(img, 9, ty, skin_d);
    }

    // arms (short striped sleeves, skin forearms)
    let sleeve_l = |img: &mut RgbaImage, y: i32| {
        rect(img, 1, y, 2, 3, RED);
        px(img, 1, y + 2, BLACK);
    };
    let sleeve_r = |img: &mut RgbaImage, y: i32| {
        rect(img, 15, y, 2, 3, RED);
        px(img, 16, y + 2, BLACK);
    };
    match m {
        Mood::Alert { left } => {
            // point toward the alert with one arm, the other down
            if left {
                rect(img, -3, ty + 1, 5, 2, RED);
                rect(img, -5, ty + 1, 2, 2, l.skin);
                sleeve_r(img, ty);
                rect(img, 15, ty + 3, 2, 4, l.skin);
            } else {
                rect(img, 16, ty + 1, 5, 2, RED);
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
            rect(img, 5, ty + 8, 6, 1, rgb(0xd0202a));
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
            rect(img, 0, ty - 4, 2, 5, RED);
            rect(img, 16, ty - 4, 2, 5, RED);
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
}
