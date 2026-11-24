//! Room + desk-pod layout in logical pixels (1×, 16 px tiles).
use office_core::{model::AgentKind, Agent, AgentId, Scene};
use std::collections::HashMap;

pub const T: i32 = 16;
pub const CELL_W: i32 = 48;
pub const CELL_H: i32 = 64;
pub const POD_W: i32 = CELL_W * 2;
pub const POD_H: i32 = CELL_H * 2;
pub const GAP_X: i32 = 16;
pub const GAP_Y: i32 = 8;
pub const MARGIN: i32 = 16;
pub const WALL_H: i32 = 48;
pub const DECOR_H: i32 = 28;
pub const BOTTOM_H: i32 = 36;
pub const MIN_W: i32 = 272;
pub const DESKS_PER_POD: usize = 4;
/// The CEO's corner office (top-right, below the decor strip).
pub const CEO_W: i32 = 72;
pub const CEO_H: i32 = 64;

/// Optional room features that change the geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extras {
    /// Reserve the CEO corner office.
    pub ceo: bool,
}

impl Default for Extras {
    fn default() -> Self {
        Extras { ceo: true }
    }
}

/// Where the office cat likes to hang out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpotKind {
    Sofa,
    Sill,
    /// On top of a desk (pod index, desk index).
    Desk(usize, usize),
    CeoDesk,
    Floor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub kind: SpotKind,
    /// Feet position.
    pub x: i32,
    pub y: i32,
}

impl Spot {
    /// Up on furniture (the cat hops to get there).
    pub fn elevated(&self) -> bool {
        !matches!(self.kind, SpotKind::Floor)
    }
}

/// Feet position of the cat sitting on desk cell (ox, oy) (between the monitor and the lamp).
pub fn desk_top(ox: i32, oy: i32) -> (i32, i32) {
    (ox + 28, oy + 34)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PodKind {
    /// Owned by a top-level Claude session (desk 0 = the session).
    Session,
    /// Extra desks for a session's interns.
    Overflow,
    /// Unmatched agy / other agents.
    Lab,
}

#[derive(Debug, Clone)]
pub struct Pod {
    pub kind: PodKind,
    pub x: i32,
    pub y: i32,
    pub desks: [Option<AgentId>; DESKS_PER_POD],
}

impl Pod {
    /// Top-left of desk cell `i` (0 TL, 1 TR, 2 BL, 3 BR).
    pub fn cell(&self, i: usize) -> (i32, i32) {
        (self.x + (i as i32 % 2) * CELL_W, self.y + (i as i32 / 2) * CELL_H)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seat {
    pub pod: usize,
    pub desk: usize,
    /// Cell top-left.
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pub cols: usize,
    pub rows: usize,
    pub pods: Vec<Pod>,
    pub seats: HashMap<AgentId, Seat>,
    /// Feet position at the door (walk start/end).
    pub door: (i32, i32),
    /// The CEO corner office [x, y, w, h], when reserved.
    pub ceo: Option<[i32; 4]>,
    /// Cat waypoints (sofa, windowsill, CEO desk, floor, then every desk top).
    pub spots: Vec<Spot>,
}

impl Layout {
    /// Key for caching static layers (depends only on room geometry and pod kinds).
    pub fn static_key(&self) -> String {
        let mut k = format!("{}x{}:{}x{}", self.w, self.h, self.cols, self.rows);
        for p in &self.pods {
            k.push_str(&format!("|{:?}@{},{}", p.kind, p.x, p.y));
        }
        if let Some(c) = self.ceo {
            k.push_str(&format!("|ceo@{c:?}"));
        }
        k
    }
}

fn session_of<'a>(scene: &'a Scene, a: &'a Agent) -> Option<&'a AgentId> {
    // Walk up the parent chain to a top-level Claude session.
    let mut cur = a;
    for _ in 0..8 {
        match &cur.parent {
            Some(p) => match scene.agents.get(p) {
                Some(pa) if pa.kind == AgentKind::Claude && pa.parent.is_none() => return Some(&pa.id),
                Some(pa) => cur = pa,
                None => return None,
            },
            None => return None,
        }
    }
    None
}

/// Group agents: (kind, members) with each session's own pods first, then the lab.
pub fn groups(scene: &Scene) -> Vec<(AgentId, Vec<AgentId>)> {
    let mut agents: Vec<&Agent> = scene.agents.values().collect();
    agents.sort_by(|a, b| a.started_ms.cmp(&b.started_ms).then(a.id.cmp(&b.id)));
    let sessions: Vec<&Agent> =
        agents.iter().copied().filter(|a| a.kind == AgentKind::Claude && a.parent.is_none()).collect();
    let mut members: Vec<(AgentId, Vec<AgentId>)> = sessions.iter().map(|s| (s.id.clone(), vec![s.id.clone()])).collect();
    let mut lab = Vec::new();
    for a in &agents {
        if a.kind == AgentKind::Claude && a.parent.is_none() {
            continue;
        }
        let mut host = session_of(scene, a).cloned();
        if host.is_none() && a.kind == AgentKind::Agy {
            if let Some(cwd) = &a.cwd {
                host = sessions.iter().find(|s| s.cwd.as_ref() == Some(cwd)).map(|s| s.id.clone());
            }
        }
        match host.and_then(|h| members.iter_mut().find(|(s, _)| *s == h)) {
            Some((_, m)) => m.push(a.id.clone()),
            None => lab.push(a.id.clone()),
        }
    }
    if !lab.is_empty() {
        members.push((String::new(), lab));
    }
    members
}

pub fn room_size(cols: usize, rows: usize) -> (i32, i32) {
    room_size_with(cols, rows, Extras::default())
}

pub fn room_size_with(cols: usize, rows: usize, ex: Extras) -> (i32, i32) {
    let cols = cols.max(1) as i32;
    let rows = rows.max(1) as i32;
    let ceo_w = if ex.ceo { GAP_X + CEO_W } else { 0 };
    let w = (MARGIN * 2 + cols * POD_W + (cols - 1) * GAP_X + ceo_w).max(MIN_W);
    let h = (WALL_H + DECOR_H + rows * POD_H + (rows - 1) * GAP_Y + BOTTOM_H).max(WALL_H + DECOR_H + CEO_H + BOTTOM_H);
    (w, h)
}

/// Pick the pod grid that gives the largest scale for a px_w × px_h target.
pub fn best_cols(n_pods: usize, px_w: u32, px_h: u32) -> usize {
    best_cols_with(n_pods, px_w, px_h, Extras::default())
}

pub fn best_cols_with(n_pods: usize, px_w: u32, px_h: u32, ex: Extras) -> usize {
    let n = n_pods.max(1);
    let mut best = (1usize, f32::MIN);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let (w, h) = room_size_with(cols, rows, ex);
        let fit = (px_w as f32 / w as f32).min(px_h as f32 / h as f32);
        if fit > best.1 + 1e-4 {
            best = (cols, fit);
        }
    }
    best.0
}

pub fn build(scene: &Scene, px_w: u32, px_h: u32) -> Layout {
    build_with(scene, px_w, px_h, Extras::default())
}

pub fn build_with(scene: &Scene, px_w: u32, px_h: u32, ex: Extras) -> Layout {
    let mut pod_plans: Vec<(PodKind, Vec<AgentId>)> = Vec::new();
    for (host, ids) in groups(scene) {
        let lab = host.is_empty();
        for (i, chunk) in ids.chunks(DESKS_PER_POD).enumerate() {
            let kind = if lab { PodKind::Lab } else if i == 0 { PodKind::Session } else { PodKind::Overflow };
            pod_plans.push((kind, chunk.to_vec()));
        }
    }
    if pod_plans.is_empty() {
        pod_plans.push((PodKind::Session, Vec::new()));
    }
    let cols = best_cols_with(pod_plans.len(), px_w, px_h, ex);
    let rows = pod_plans.len().div_ceil(cols);
    let (w, h) = room_size_with(cols, rows, ex);
    // centre the pod grid horizontally (left of the CEO corner, when reserved)
    let grid_w = cols as i32 * POD_W + (cols as i32 - 1) * GAP_X;
    let ceo = ex.ceo.then(|| [w - MARGIN - CEO_W, WALL_H + DECOR_H, CEO_W, CEO_H]);
    let x0 = match ceo {
        Some(c) => MARGIN + (c[0] - GAP_X - MARGIN - grid_w).max(0) / 2,
        None => (w - grid_w) / 2,
    };
    let mut pods = Vec::new();
    let mut seats = HashMap::new();
    for (i, (kind, ids)) in pod_plans.into_iter().enumerate() {
        let (c, r) = ((i % cols) as i32, (i / cols) as i32);
        let mut pod = Pod {
            kind,
            x: x0 + c * (POD_W + GAP_X),
            y: WALL_H + DECOR_H + r * (POD_H + GAP_Y),
            desks: Default::default(),
        };
        for (d, id) in ids.into_iter().enumerate() {
            let (x, y) = pod.cell(d);
            seats.insert(id.clone(), Seat { pod: i, desk: d, x, y });
            pod.desks[d] = Some(id);
        }
        pods.push(pod);
    }
    let mut spots = vec![
        Spot { kind: SpotKind::Sofa, x: w - SOFA_CX_FROM_RIGHT, y: WALL_H + DECOR_H - 9 },
        Spot { kind: SpotKind::Sill, x: 28, y: SILL_Y },
        Spot { kind: SpotKind::Floor, x: MARGIN + 30, y: WALL_H + DECOR_H + 2 },
    ];
    if let Some([cx, cy, cw, _]) = ceo {
        spots.push(Spot { kind: SpotKind::CeoDesk, x: cx + cw - 16, y: cy + CEO_DESK_TOP + 2 });
    }
    for (pi, pod) in pods.iter().enumerate() {
        for d in 0..DESKS_PER_POD {
            let (ox, oy) = pod.cell(d);
            let (x, y) = desk_top(ox, oy);
            spots.push(Spot { kind: SpotKind::Desk(pi, d), x, y });
        }
    }
    Layout { w, h, cols, rows, pods, seats, door: (MARGIN + 9, h - 14), ceo, spots }
}

/// Sofa centre, measured from the right wall (the renderer centres the sofa sprite here).
pub const SOFA_CX_FROM_RIGHT: i32 = 46;
/// Feet y of a cat sitting on the first window's sill.
pub const SILL_Y: i32 = 30;
/// Top of the CEO desk surface, relative to the corner office top.
pub const CEO_DESK_TOP: i32 = 34;

#[cfg(test)]
mod tests {
    use super::*;
    use office_core::AgentState;

    pub fn agent(id: &str, kind: AgentKind, parent: Option<&str>, cwd: Option<&str>, t: u64) -> Agent {
        Agent {
            id: id.into(),
            kind,
            label: id.into(),
            parent: parent.map(Into::into),
            cwd: cwd.map(Into::into),
            role: None,
            model: None,
            slot: None,
            pid: None,
            state: AgentState::Idle,
            activity: String::new(),
            started_ms: t,
            last_event_ms: t,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
            context_pct: None,
            rss_mb: None,
            tool_calls: 0,
            tool_failures: 0,
            resume_id: None,
            pending_permission: None,
        }
    }

    fn scene(v: Vec<Agent>) -> Scene {
        let mut s = Scene::default();
        for a in v {
            s.agents.insert(a.id.clone(), a);
        }
        s
    }

    fn no_overlap(l: &Layout) {
        let rects: Vec<(i32, i32)> = l.seats.values().map(|s| (s.x, s.y)).collect();
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap = (a.0 - b.0).abs() < CELL_W && (a.1 - b.1).abs() < CELL_H;
                assert!(!overlap, "desks overlap: {a:?} {b:?}");
            }
            assert!(a.0 >= 0 && a.1 >= WALL_H && a.0 + CELL_W <= l.w && a.1 + CELL_H <= l.h - BOTTOM_H + GAP_Y);
        }
        // pods don't overlap either
        for (i, p) in l.pods.iter().enumerate() {
            for q in &l.pods[i + 1..] {
                assert!((p.x - q.x).abs() >= POD_W || (p.y - q.y).abs() >= POD_H);
            }
        }
    }

    #[test]
    fn n_agents_never_overlap() {
        for n in 0..40usize {
            let mut v = Vec::new();
            for s in 0..(n / 5 + 1).min(n) {
                v.push(agent(&format!("cc:{s}"), AgentKind::Claude, None, Some(&format!("/p/{s}")), s as u64));
            }
            let sessions = v.len();
            for i in sessions..n {
                let kind = if i % 3 == 0 { AgentKind::Agy } else { AgentKind::ClaudeSub };
                let parent = if kind == AgentKind::ClaudeSub && sessions > 0 { Some(format!("cc:{}", i % sessions)) } else { None };
                v.push(agent(&format!("a:{i}"), kind, parent.as_deref(), Some("/elsewhere"), 100 + i as u64));
            }
            for (pw, ph) in [(640, 360), (1600, 900), (300, 800)] {
                let l = build(&scene(v.clone()), pw, ph);
                assert_eq!(l.seats.len(), n, "every agent seated (n={n})");
                no_overlap(&l);
            }
        }
    }

    #[test]
    fn ceo_corner_is_reserved_without_overlap() {
        for n in 0..40usize {
            let mut v = Vec::new();
            for s in 0..(n / 4 + 1).min(n) {
                v.push(agent(&format!("cc:{s}"), AgentKind::Claude, None, Some(&format!("/p/{s}")), s as u64));
            }
            let sessions = v.len();
            for i in sessions..n {
                let kind = if i % 2 == 0 { AgentKind::Agy } else { AgentKind::ClaudeSub };
                let parent = if kind == AgentKind::ClaudeSub && sessions > 0 { Some(format!("cc:{}", i % sessions)) } else { None };
                v.push(agent(&format!("a:{i}"), kind, parent.as_deref(), None, 100 + i as u64));
            }
            for (pw, ph) in [(640, 360), (1600, 900), (300, 800), (1920, 400)] {
                let l = build(&scene(v.clone()), pw, ph);
                let [cx, cy, cw, ch] = l.ceo.expect("CEO corner reserved");
                assert!(cx >= 0 && cy >= WALL_H && cx + cw <= l.w && cy + ch <= l.h - BOTTOM_H, "CEO corner inside the room (n={n})");
                for p in &l.pods {
                    let apart = p.x + POD_W <= cx || p.x >= cx + cw || p.y + POD_H <= cy || p.y >= cy + ch;
                    assert!(apart, "pod {:?} overlaps the CEO corner {:?} (n={n}, {pw}x{ph})", (p.x, p.y), (cx, cy));
                }
                assert_eq!(l.seats.len(), n);
                no_overlap(&l);
                for sp in &l.spots {
                    assert!(sp.x >= 0 && sp.y >= 0 && sp.x < l.w && sp.y < l.h, "spot {sp:?} outside the room");
                }
                assert!(l.spots.iter().any(|s| s.kind == SpotKind::CeoDesk));
                assert!(l.spots.iter().any(|s| s.kind == SpotKind::Sofa));
            }
        }
        let off = build_with(&Scene::default(), 640, 360, Extras { ceo: false });
        assert!(off.ceo.is_none() && !off.spots.iter().any(|s| s.kind == SpotKind::CeoDesk));
    }

    #[test]
    fn interns_join_their_session_and_lab_collects_the_rest() {
        let s = scene(vec![
            agent("cc:a", AgentKind::Claude, None, Some("/p/a"), 1),
            agent("cc:b", AgentKind::Claude, None, Some("/p/b"), 2),
            agent("cc-sub:a/1", AgentKind::ClaudeSub, Some("cc:a"), None, 3),
            agent("agy:1", AgentKind::Agy, None, Some("/p/b"), 4),
            agent("agy:2", AgentKind::Agy, None, Some("/nowhere"), 5),
            agent("x:1", AgentKind::Other, None, None, 6),
        ]);
        let l = build(&s, 640, 360);
        assert_eq!(l.pods.len(), 3);
        assert_eq!(l.seats["cc:a"].pod, l.seats["cc-sub:a/1"].pod);
        assert_eq!(l.seats["cc:b"].pod, l.seats["agy:1"].pod);
        assert_eq!(l.seats["cc:a"].desk, 0);
        assert_eq!(l.pods[l.seats["agy:2"].pod].kind, PodKind::Lab);
        assert_eq!(l.seats["agy:2"].pod, l.seats["x:1"].pod);
    }

    #[test]
    fn overflow_adds_pods_and_rows() {
        let mut v = vec![agent("cc:a", AgentKind::Claude, None, None, 0)];
        for i in 0..9 {
            v.push(agent(&format!("s{i}"), AgentKind::ClaudeSub, Some("cc:a"), None, 1 + i));
        }
        let l = build(&scene(v), 400, 900);
        assert_eq!(l.pods.len(), 3);
        assert_eq!(l.pods[1].kind, PodKind::Overflow);
        assert!(l.rows >= 2, "tall target stacks rows");
    }
}
