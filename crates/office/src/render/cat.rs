//! The office cat: an ambient pet (not an agent), drawn as original procedural pixel art (about
//! 12×8 at 1×). Its behaviour is a pure function of time and the scene (no RNG state): every
//! `LEG_MS` it picks a new waypoint (sofa, windowsill, CEO desk, a desk, the floor), walks or hops
//! there, then sits, grooms, loafs or sleeps. When an agent is in NeedsYou/Error it sometimes hops
//! onto that agent's desk, as if it were alerting you.
use super::canvas::{fill, put, rgb, shade, Col};
use super::layout::{desk_top, Layout, Spot, SpotKind};
use image::RgbaImage;
use office_core::{AgentState, Scene};

/// One "leg" of the cat's day: walk to a waypoint, then idle there.
pub const LEG_MS: u64 = 14_000;
const WALK_PX_PER_S: f32 = 32.0;
const MAX_WALK_MS: f32 = 7_000.0;
/// Sprite box (feet at the bottom centre).
pub const BOX_W: i32 = 12;
pub const BOX_H: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Walk cycle frame 0..4.
    Walk(u8),
    Sit,
    /// Licking a paw (bool = paw up).
    Groom(bool),
    /// Curled up asleep (zZ).
    Sleep,
    /// Sitting on an alerting agent's desk, staring at you.
    Alert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatState {
    /// Feet position (logical px).
    pub x: i32,
    pub y: i32,
    pub left: bool,
    pub action: Action,
    /// Tail flicked up this frame.
    pub tail_up: bool,
}

fn mix(mut z: u64) -> u64 {
    // splitmix64 finaliser
    z = z.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Feet positions of desks whose agent needs you (or errored), in a stable order.
fn alert_desks(scene: &Scene, lay: &Layout) -> Vec<Spot> {
    let mut v: Vec<(&String, Spot)> = scene
        .agents
        .values()
        .filter(|a| matches!(a.state, AgentState::NeedsYou | AgentState::Error))
        .filter_map(|a| {
            let s = lay.seats.get(&a.id)?;
            let (x, y) = desk_top(s.x, s.y);
            Some((&a.id, Spot { kind: SpotKind::Desk(s.pod, s.desk), x, y }))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.into_iter().map(|(_, s)| s).collect()
}

/// Waypoint for leg `k` (and whether it is an alert visit).
fn waypoint(lay: &Layout, alerts: &[Spot], k: u64) -> (Spot, bool) {
    let h = mix(k);
    if !alerts.is_empty() && h % 3 == 0 {
        return (alerts[((h >> 8) % alerts.len() as u64) as usize], true);
    }
    let find = |kind: SpotKind| lay.spots.iter().copied().find(|s| s.kind == kind);
    let desks: Vec<Spot> = lay.spots.iter().copied().filter(|s| matches!(s.kind, SpotKind::Desk(..))).collect();
    let fallback = lay.spots.first().copied().unwrap_or(Spot { kind: SpotKind::Floor, x: lay.w / 2, y: lay.h / 2 });
    let spot = match (h >> 16) % 10 {
        0..=2 => find(SpotKind::Sofa),
        3 | 4 => find(SpotKind::Sill),
        5 | 6 => find(SpotKind::CeoDesk).or_else(|| find(SpotKind::Floor)),
        7 => find(SpotKind::Floor),
        _ if !desks.is_empty() => Some(desks[((h >> 32) % desks.len() as u64) as usize]),
        _ => None,
    };
    (spot.unwrap_or(fallback), false)
}

/// One leg of the cat's day: from one waypoint to the next, starting at `start` (ms).
#[derive(Debug, Clone, PartialEq)]
pub struct Leg {
    pub k: u64,
    pub start: u64,
    pub from: Spot,
    pub to: Spot,
    pub alert: bool,
    /// The alerting desks when this leg was planned.
    alerts: Vec<Spot>,
    /// Layout geometry the leg was planned on.
    layout: String,
}

impl Leg {
    fn plan(lay: &Layout, alerts: &[Spot], k: u64, start: u64, from: Spot) -> Leg {
        let (to, alert) = waypoint(lay, alerts, k);
        Leg { k, start, from, to, alert, alerts: alerts.to_vec(), layout: lay.static_key() }
    }

    /// The leg a fresh cat would be on at `t` (a pure function of `t`).
    fn fresh(lay: &Layout, alerts: &[Spot], t: u64) -> Leg {
        let k = t / LEG_MS + 1;
        let (from, _) = waypoint(lay, alerts, k - 1);
        Leg::plan(lay, alerts, k, (k - 1) * LEG_MS, from)
    }

    fn walk_ms(&self, lay: &Layout) -> f32 {
        (path_len(&lay.route(&self.from, &self.to)) / WALK_PX_PER_S * 1000.0).min(MAX_WALK_MS)
    }
}

/// The cat's memory between frames, so a change in the alerts (or the clock) never makes it
/// teleport: a walk in progress always finishes before the cat retargets.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    leg: Option<Leg>,
}

impl Memory {
    /// Where the cat is and what it is doing at `t`, continuing the current leg.
    pub fn state(&mut self, lay: &Layout, scene: &Scene, t: u64) -> CatState {
        let alerts = alert_desks(scene, lay);
        let key = lay.static_key();
        let mut leg = match self.leg.take() {
            // a new layout, a clock jump backwards or a long gap: start over
            Some(l) if l.layout == key && t >= l.start && t < l.start + 2 * LEG_MS => l,
            _ => Leg::fresh(lay, &alerts, t),
        };
        let walked = t as f32 >= leg.start as f32 + leg.walk_ms(lay);
        if t >= leg.start + LEG_MS {
            let start = leg.start + LEG_MS;
            leg = Leg::plan(lay, &alerts, leg.k + 1, start.max(t.saturating_sub(LEG_MS - 1)), leg.to);
        } else if walked && leg.alerts != alerts {
            // the alerts changed: once there, head off at once (to the new alert, or away)
            leg = Leg::plan(lay, &alerts, leg.k + 1, t, leg.to);
        }
        let st = leg_state(lay, &leg, t);
        self.leg = Some(leg);
        st
    }
}

/// Where a fresh cat is and what it is doing at time `t` (pure; the renderer uses `Memory`).
pub fn state(lay: &Layout, scene: &Scene, t: u64) -> CatState {
    let alerts = alert_desks(scene, lay);
    leg_state(lay, &Leg::fresh(lay, &alerts, t), t)
}

fn path_len(pts: &[(i32, i32)]) -> f32 {
    pts.windows(2).map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as f32).sum()
}

/// Feet position at progress `p` (0..=1) along the route, with hop arcs on the first segment
/// (leaving furniture) and the last one (reaching it). Also returns the segment index and its
/// horizontal direction.
pub fn walk_pos(lay: &Layout, from: &Spot, to: &Spot, p: f32) -> (f32, f32, usize, i32) {
    pos_on(&lay.route(from, to), from, to, p)
}

fn pos_on(pts: &[(i32, i32)], from: &Spot, to: &Spot, p: f32) -> (f32, f32, usize, i32) {
    let total = path_len(pts).max(1.0);
    let mut d = p.clamp(0.0, 1.0) * total;
    let last = pts.len().saturating_sub(2);
    for (i, w) in pts.windows(2).enumerate() {
        let len = ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) as f32;
        if d <= len || i == last {
            let q = if len > 0.0 { (d / len).clamp(0.0, 1.0) } else { 1.0 };
            let x = w[0].0 as f32 + (w[1].0 - w[0].0) as f32 * q;
            let mut y = w[0].1 as f32 + (w[1].1 - w[0].1) as f32 * q;
            let hop = (i == 0 && from.elevated()) || (i == last && to.elevated());
            if hop {
                y -= (std::f32::consts::PI * q).sin() * 7.0;
            }
            return (x, y, i, (w[1].0 - w[0].0).signum());
        }
        d -= len;
    }
    (to.x as f32, to.y as f32, last, 0)
}

fn leg_state(lay: &Layout, leg: &Leg, t: u64) -> CatState {
    let (from, to, alert, k) = (leg.from, leg.to, leg.alert, leg.k);
    let within = t.saturating_sub(leg.start);
    let walk_ms = leg.walk_ms(lay);
    let dx = to.x - from.x;
    let tail_up = (t / 450) % 7 == 0 || (alert && (t / 250) % 2 == 0);
    let default_left = if dx != 0 { dx < 0 } else { mix(k ^ 0x51) % 2 == 0 };
    if (within as f32) < walk_ms {
        let p = within as f32 / walk_ms;
        let (x, y, _, sx) = walk_pos(lay, &from, &to, p);
        let left = if sx != 0 { sx < 0 } else { default_left };
        let frame = ((t / 110) % 4) as u8;
        return clamp_in(lay, CatState { x: x.round() as i32, y: y.round() as i32, left, action: Action::Walk(frame), tail_up });
    }
    let left = default_left;
    let rest = within as f32 - walk_ms;
    let action = if alert {
        Action::Alert
    } else if to.kind == SpotKind::Sofa {
        if rest < 1_500.0 { Action::Groom((t / 280) % 2 == 0) } else { Action::Sleep }
    } else {
        match mix(k ^ 0xCA7) % 4 {
            0 => Action::Sleep,
            1 if rest < 5_000.0 => Action::Groom((t / 280) % 2 == 0),
            _ => Action::Sit,
        }
    };
    clamp_in(lay, CatState { x: to.x, y: to.y, left, action, tail_up })
}

fn clamp_in(lay: &Layout, mut s: CatState) -> CatState {
    s.x = s.x.clamp(BOX_W / 2, lay.w - BOX_W / 2);
    s.y = s.y.clamp(BOX_H, lay.h - 1);
    s
}

/// Body colours for a config colour name.
#[derive(Debug, Clone, Copy)]
pub struct Coat {
    pub base: Col,
    pub stripe: Col,
    pub light: Col,
    pub eye: Col,
    /// Calico: patches alternate between `stripe` and this.
    pub patch: Option<Col>,
}

pub fn coat(name: &str) -> Coat {
    let c = |b, s, l, e| Coat { base: rgb(b), stripe: rgb(s), light: rgb(l), eye: rgb(e), patch: None };
    match name.trim().to_ascii_lowercase().as_str() {
        "orange" | "ginger" => c(0xe3893a, 0xb35f1e, 0xf6d9b0, 0x7cc243),
        "black" => c(0x2c2c32, 0x1a1a1e, 0x4a4a52, 0xf2d23c),
        "white" => c(0xf1f0ec, 0xd2d0c8, 0xffffff, 0x5fa8e8),
        "calico" => Coat { patch: Some(rgb(0xe08a3c)), ..c(0xf3efe6, 0x2e2a28, 0xffffff, 0x9ad14b) },
        _ => c(0x8e9196, 0x5c5f65, 0xd5d7da, 0x9ad14b),
    }
}

// Pixel maps, right-facing, BOX_W × BOX_H, feet on the last row.
// B base, S stripe/patch, L light, E eye, N nose, Z closed eye (stripe colour), . clear.
const WALK: [&str; 10] = [
    "............",
    "............",
    "S.......B..B",
    "S.......BBBB",
    ".S......BBEB",
    ".SBSBSBSBBLN",
    "..BBBBBBBBL.",
    "..LLLLLLBB..",
    "............", // legs row 1 (per frame)
    "............", // legs row 2
];
/// Leg columns per walk frame: (upper, lower) for 4 legs.
const LEGS: [[(i32, i32); 4]; 4] = [
    [(2, 2), (4, 4), (7, 7), (9, 9)],
    [(3, 3), (4, 4), (8, 8), (9, 9)],
    [(2, 1), (4, 5), (7, 6), (9, 10)],
    [(3, 3), (4, 4), (8, 8), (9, 9)],
];
const SIT: [&str; 10] = [
    "......B..B..",
    "......BBBB..",
    "......EBBE..",
    "......BLNL..",
    ".....SBBBBS.",
    "....SBLLLBS.",
    "...SBBLLLBB.",
    "..SBBBLLLBB.",
    "..BBBBLLLBB.",
    "..SSBBL.LBB.",
];
const SLEEP: [&str; 10] = [
    "............",
    "............",
    "............",
    "............",
    "........B..B",
    "...SBSBSBBBB",
    "..SBSBSBBZBZ",
    ".BBBBBBBBLNL",
    "SBBBBBBBBBB.",
    "SSLLLLLLLLL.",
];

/// Draw the cat (feet at (x, y)) into `img` (unthemed; the caller applies fx).
pub fn draw(img: &mut RgbaImage, x: i32, y: i32, st: &CatState, coat: &Coat) {
    let map: &[&str; 10] = match st.action {
        Action::Walk(_) => &WALK,
        Action::Sleep => &SLEEP,
        _ => &SIT,
    };
    let ox = x - BOX_W / 2;
    let oy = y - BOX_H;
    let col_x = |c: i32| if st.left { ox + BOX_W - 1 - c } else { ox + c };
    let stripe = |c: i32, r: i32| match coat.patch {
        Some(p) if ((c / 3) + (r / 2)) % 2 == 0 => p,
        _ => coat.stripe,
    };
    for (r, row) in map.iter().enumerate() {
        for (c, ch) in row.bytes().enumerate() {
            let (c, r) = (c as i32, r as i32);
            let col = match ch {
                b'B' => coat.base,
                b'S' => stripe(c, r),
                b'L' => coat.light,
                b'E' => coat.eye,
                b'N' => rgb(0xe59aa6),
                b'Z' => shade(coat.stripe, 0.8),
                _ => continue,
            };
            put(img, col_x(c), oy + r, col);
        }
    }
    match st.action {
        Action::Walk(f) => {
            for (i, (a, b)) in LEGS[f as usize % 4].iter().enumerate() {
                let c = if i % 2 == 0 { shade(coat.base, 0.82) } else { coat.base };
                put(img, col_x(*a), oy + 8, c);
                put(img, col_x(*b), oy + 9, coat.light);
            }
            // tail tip flick while walking
            if st.tail_up {
                put(img, col_x(1), oy + 1, stripe(1, 1));
            }
        }
        Action::Sit | Action::Alert | Action::Groom(_) => {
            // tail: curled on the floor, or flicked up
            if st.tail_up {
                put(img, col_x(1), oy + 6, stripe(1, 6));
                put(img, col_x(1), oy + 7, stripe(1, 7));
                put(img, col_x(2), oy + 8, stripe(2, 8));
            } else {
                fill_row(img, col_x, oy + 9, &[0, 1], stripe(0, 9));
            }
            if let Action::Groom(up) = st.action {
                // eyes closed, paw to the mouth
                put(img, col_x(6), oy + 2, shade(coat.base, 0.7));
                put(img, col_x(9), oy + 2, shade(coat.base, 0.7));
                if up {
                    put(img, col_x(8), oy + 4, coat.light);
                    put(img, col_x(8), oy + 5, coat.light);
                }
            }
        }
        Action::Sleep => {
            if st.tail_up {
                put(img, col_x(0), oy + 7, stripe(0, 7));
            }
        }
    }
}

fn fill_row(img: &mut RgbaImage, col_x: impl Fn(i32) -> i32, y: i32, cols: &[i32], c: Col) {
    for k in cols {
        fill(img, col_x(*k), y, 1, 1, c);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{demo, layout};
    use super::*;

    #[test]
    fn cat_always_inside_the_room() {
        let now = 10_000_000;
        let scenes = [demo::demo_scene(now), Scene::default()];
        for s in &scenes {
            for (pw, ph, ceo) in [(640, 360, true), (1600, 900, false), (300, 800, true)] {
                let lay = layout::build_with(s, pw, ph, layout::Extras { ceo });
                let mut seen = std::collections::HashSet::new();
                for i in 0..4_000u64 {
                    let t = now + i * 97;
                    let c = state(&lay, s, t);
                    assert!(c.x - BOX_W / 2 >= 0 && c.x + BOX_W / 2 <= lay.w, "cat x {c:?} outside {}", lay.w);
                    assert!(c.y - BOX_H >= 0 && c.y < lay.h, "cat y {c:?} outside {}", lay.h);
                    seen.insert(std::mem::discriminant(&c.action));
                    assert_eq!(c, state(&lay, s, t), "deterministic");
                }
                assert!(seen.len() >= 3, "the cat does several things: {seen:?}");
            }
        }
    }

    #[test]
    fn cat_visits_alerting_desks() {
        let now = 10_000_000;
        let s = demo::demo_scene(now);
        let lay = layout::build(&s, 640, 360);
        let alerts = alert_desks(&s, &lay);
        assert!(!alerts.is_empty());
        let visits = (0..300u64).filter(|k| waypoint(&lay, &alerts, *k).1).count();
        assert!(visits > 30, "alert visits: {visits}");
        let none = alert_desks(&Scene::default(), &lay);
        assert!((0..300u64).all(|k| !waypoint(&lay, &none, k).1));
    }

    /// Layouts to exercise: the demo office, an empty one, and crowded ones, with/without the CEO.
    fn layouts() -> Vec<(Scene, layout::Layout)> {
        let now = 10_000_000;
        let mut out = Vec::new();
        let mut big = demo::demo_scene(now);
        for i in 0..14 {
            let mut a = big.agents.values().next().unwrap().clone();
            a.id = format!("extra:{i}");
            a.parent = None;
            a.kind = office_core::model::AgentKind::Claude;
            a.state = if i % 3 == 0 { AgentState::NeedsYou } else { AgentState::Typing };
            a.started_ms = now + i;
            big.agents.insert(a.id.clone(), a);
        }
        for s in [demo::demo_scene(now), Scene::default(), big] {
            for (pw, ph, ceo) in [(640, 360, true), (1600, 900, false), (300, 800, true), (1920, 400, true)] {
                let lay = layout::build_with(&s, pw, ph, layout::Extras { ceo });
                out.push((s.clone(), lay));
            }
        }
        out
    }

    #[test]
    fn cat_routes_never_cross_desks_or_the_nameplate() {
        for (s, lay) in layouts() {
            let obstacles = lay.obstacles();
            let mut spots = lay.spots.clone();
            spots.extend(alert_desks(&s, &lay));
            for from in &spots {
                for to in &spots {
                    let pts = lay.route(from, to);
                    let last = pts.len().saturating_sub(2);
                    let n = (path_len(&pts) as usize).max(1);
                    for i in 0..=n {
                        let (x, y, seg, _) = pos_on(&pts, from, to, i as f32 / n as f32);
                        let (x, y) = (x.round() as i32, y.round() as i32);
                        for o in obstacles.iter().filter(|o| o.contains(x, y)) {
                            // only the deliberate hops: off the start desk, onto the target desk
                            let ok = (seg == last && o.spot == Some(to.kind)) || (seg == 0 && o.spot == Some(from.kind));
                            assert!(ok, "cat at {:?} (segment {seg}/{last}) inside {o:?} on {from:?} -> {to:?} via {pts:?}", (x, y));
                        }
                    }
                    // Manhattan: every segment is horizontal or vertical
                    assert!(pts.windows(2).all(|w| w[0].0 == w[1].0 || w[0].1 == w[1].1), "{pts:?}");
                }
            }
        }
    }

    #[test]
    fn no_teleport_when_alerts_start_or_clear() {
        let now = 10_000_000;
        let alerting = demo::demo_scene(now);
        let mut calm = alerting.clone();
        for a in calm.agents.values_mut() {
            if matches!(a.state, AgentState::NeedsYou | AgentState::Error) {
                a.state = AgentState::Idle;
            }
        }
        let lay = layout::build(&alerting, 640, 360);
        assert_eq!(lay.static_key(), layout::build(&calm, 640, 360).static_key());
        let mut mem = Memory::default();
        let mut prev: Option<CatState> = None;
        let mut alert_frames = 0;
        for i in 0..6_000u64 {
            let t = now + i * 50;
            // alerts flip on and off at odd moments, often mid-walk
            let s = if (t / 3_700) % 2 == 0 { &alerting } else { &calm };
            let c = mem.state(&lay, s, t);
            if let Some(p) = prev {
                let jump = (c.x - p.x).abs() + (c.y - p.y).abs();
                assert!(jump <= 24, "teleport {p:?} -> {c:?} at t={t}");
            }
            if c.action == Action::Alert {
                alert_frames += 1;
            }
            prev = Some(c);
        }
        assert!(alert_frames > 0, "the cat still answers alerts");
    }

    #[test]
    fn every_coat_draws() {
        for name in ["grey", "orange", "black", "white", "calico", "nonsense"] {
            for action in [Action::Walk(2), Action::Sit, Action::Groom(true), Action::Sleep, Action::Alert] {
                let mut img = RgbaImage::new(20, 16);
                let st = CatState { x: 10, y: 14, left: true, action, tail_up: true };
                draw(&mut img, 10, 14, &st, &coat(name));
                assert!(img.pixels().any(|p| p.0[3] > 0));
            }
        }
    }
}
