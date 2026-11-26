//! Room + desk-pod layout in logical pixels (1×, 16 px tiles).
use office_core::{model::AgentKind, Agent, AgentId, Scene};
use std::collections::HashMap;

pub const T: i32 = 16;
pub const CELL_W: i32 = 48;
pub const CELL_H: i32 = 64;
pub const POD_W: i32 = CELL_W * 2;
pub const POD_H: i32 = CELL_H * 2;
pub const GAP_X: i32 = 24;
pub const GAP_Y: i32 = 12;
/// Nameplate strip above each session's floor zone (the session name is drawn there).
pub const ZONE_HEAD: i32 = 11;
/// How far a session's floor zone extends around its pods.
pub const ZONE_PAD: i32 = 5;
pub const MARGIN: i32 = 16;
pub const WALL_H: i32 = 48;
pub const DECOR_H: i32 = 38;
pub const BOTTOM_H: i32 = 46;
pub const MIN_W: i32 = 272;
pub const DESKS_PER_POD: usize = 4;
/// Council cabin: y of the far-side seats and the gap to the near-side seats (relative to the pod).
pub const COUNCIL_ROW0: i32 = 12;
pub const COUNCIL_ROW_DY: i32 = 58;
/// The conference table [x, y, w, h] relative to the pod origin (surface + front edge).
pub const COUNCIL_TABLE: [i32; 4] = [12, 42, 72, 36];
/// The CEO's corner office (top-right, below the decor strip).
pub const CEO_W: i32 = 72;
pub const CEO_H: i32 = 64;

/// Optional room features that change the geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extras {
    /// Reserve the CEO corner office.
    pub ceo: bool,
    /// Council members get a glass cabin; when false they sit at ordinary desks.
    pub council: bool,
    /// Expand layout and scale to fill the viewport smoothly (vs integer pixel-perfect steps).
    pub fill_viewport: bool,
}

impl Default for Extras {
    fn default() -> Self {
        Extras { ceo: true, council: true, fill_viewport: false }
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
    /// A session's council cabin: council seats (architect, advisors) and their agy calls at a
    /// round table instead of desks.
    Council,
}

/// Council members: the `agy-council-*` relay subagents and their `council-*` agy calls.
pub fn is_council(a: &Agent) -> bool {
    let r = a.role.as_deref().unwrap_or("");
    r.starts_with("agy-council") || (a.kind == AgentKind::Agy && r.starts_with("council"))
}

#[derive(Debug, Clone)]
pub struct Pod {
    pub kind: PodKind,
    pub x: i32,
    pub y: i32,
    pub desks: [Option<AgentId>; DESKS_PER_POD],
    /// Index of the group (one per session, then the lab) this pod belongs to.
    pub group: usize,
}

/// A session's own patch of floor: one per run of same-group pods in a row.
#[derive(Debug, Clone)]
pub struct Zone {
    pub group: usize,
    /// The owning top-level session (None for the agy lab).
    pub session: Option<AgentId>,
    /// Floor rect [x, y, w, h], nameplate strip included.
    pub rect: [i32; 4],
    /// True for the first zone of a group (it carries the nameplate text).
    pub head: bool,
}

impl Pod {
    /// Top-left of desk cell `i` (0 TL, 1 TR, 2 BL, 3 BR).
    pub fn cell(&self, i: usize) -> (i32, i32) {
        if self.kind == PodKind::Council {
            // two seats per long side of the conference table: far side (0, 1), near side (2, 3)
            return (self.x - 2 + (i as i32 % 2) * CELL_W, self.y + COUNCIL_ROW0 + (i as i32 / 2) * COUNCIL_ROW_DY);
        }
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
    /// Per-session floor zones (drawn under the pods, each in its session's colour).
    pub zones: Vec<Zone>,
    pub seats: HashMap<AgentId, Seat>,
    /// Feet position at the door (walk start/end).
    pub door: (i32, i32),
    /// Waiting lobby area along the bottom corridor (feet y, starting x).
    pub lobby_bench: (i32, i32),
    /// Back-wall coffee bar access point.
    pub coffee_bar: (i32, i32),
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
            k.push_str(&format!("|{:?}@{},{}#{}", p.kind, p.x, p.y, p.group));
        }
        if let Some(c) = self.ceo {
            k.push_str(&format!("|ceo@{c:?}"));
        }
        k
    }

    /// Zero-allocation hash of room geometry and pod kinds.
    pub fn static_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut feed = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x100000001b3);
        };
        feed(self.w as u64);
        feed(self.h as u64);
        feed(self.cols as u64);
        feed(self.rows as u64);
        for p in &self.pods {
            feed(p.kind as u64);
            feed(p.x as u64);
            feed(p.y as u64);
            feed(p.group as u64);
        }
        if let Some([cx, cy, cw, ch]) = self.ceo {
            feed(1);
            feed(cx as u64);
            feed(cy as u64);
            feed(cw as u64);
            feed(ch as u64);
        } else {
            feed(0);
        }
        h
    }
}

fn session_of<'a>(scene: &'a Scene, a: &'a Agent) -> Option<&'a AgentId> {
    // Walk up the parent chain to a top-level Claude session.
    let mut cur = a;
    for _ in 0..8 {
        let Some(p) = &cur.parent else { return None };
        let pa = scene.agents.get(p)?;
        if pa.kind == AgentKind::Claude && pa.parent.is_none() {
            return Some(&pa.id);
        }
        cur = pa;
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
    let h = (WALL_H + DECOR_H + rows * (POD_H + ZONE_HEAD + ZONE_PAD) + (rows - 1) * GAP_Y + BOTTOM_H)
        .max(WALL_H + DECOR_H + CEO_H + BOTTOM_H);
    (w, h)
}

/// Pick the pod grid that gives the largest scale for a px_w × px_h target.
pub fn best_cols(n_pods: usize, px_w: u32, px_h: u32) -> usize {
    best_cols_with(n_pods, px_w, px_h, Extras::default())
}

pub fn best_cols_with(n_pods: usize, px_w: u32, px_h: u32, ex: Extras) -> usize {
    let n = n_pods.max(1);
    let (pw, ph) = (px_w.max(1) as f32, px_h.max(1) as f32);
    let mut best = (1usize, f32::MIN, f32::MIN);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let (w, h) = room_size_with(cols, rows, ex);
        let raw_fit = (pw / w as f32).min(ph / h as f32);
        let int_fit = if ex.fill_viewport || raw_fit < 1.0 { raw_fit } else { raw_fit.floor() };
        let fill = (w as f32 * int_fit * h as f32 * int_fit) / (pw * ph);
        if int_fit > best.1 + 1e-4 || ((int_fit - best.1).abs() <= 1e-4 && fill > best.2 + 1e-4) {
            best = (cols, int_fit, fill);
        }
    }
    best.0
}

pub fn build(scene: &Scene, px_w: u32, px_h: u32) -> Layout {
    build_with(scene, px_w, px_h, Extras::default())
}

pub fn build_with(scene: &Scene, px_w: u32, px_h: u32, ex: Extras) -> Layout {
    let mut pod_plans: Vec<(PodKind, Vec<AgentId>, usize)> = Vec::new();
    let mut group_hosts: Vec<Option<AgentId>> = Vec::new();
    for (g, (host, ids)) in groups(scene).into_iter().enumerate() {
        let lab = host.is_empty();
        group_hosts.push((!lab).then(|| host.clone()));
        let (council, desks): (Vec<AgentId>, Vec<AgentId>) = if lab || !ex.council {
            (Vec::new(), ids)
        } else {
            ids.into_iter().partition(|id| scene.agents.get(id).is_some_and(is_council))
        };
        for (i, chunk) in desks.chunks(DESKS_PER_POD).enumerate() {
            let kind = if lab { PodKind::Lab } else if i == 0 { PodKind::Session } else { PodKind::Overflow };
            pod_plans.push((kind, chunk.to_vec(), g));
        }
        for chunk in council.chunks(DESKS_PER_POD) {
            pod_plans.push((PodKind::Council, chunk.to_vec(), g));
        }
    }
    if pod_plans.is_empty() {
        pod_plans.push((PodKind::Session, Vec::new(), 0));
        group_hosts.push(None);
    }
    let cols = best_cols_with(pod_plans.len(), px_w, px_h, ex);
    let rows = pod_plans.len().div_ceil(cols);
    let (base_w, base_h) = room_size_with(cols, rows, ex);
    let fit = (px_w.max(1) as f32 / base_w as f32).min(px_h.max(1) as f32 / base_h as f32);
    let (w, h) = if ex.fill_viewport {
        if fit > 0.0 {
            let tw = ((px_w as f32 / fit).floor() as i32) & !1;
            let th = ((px_h as f32 / fit).floor() as i32) & !1;
            (tw.clamp(base_w, base_w + 180), th.clamp(base_h, base_h + 64))
        } else {
            (base_w, base_h)
        }
    } else if fit >= 1.0 {
        let s = fit.floor();
        let tw = ((px_w as f32 / s).floor() as i32) & !1;
        let th = ((px_h as f32 / s).floor() as i32) & !1;
        (tw.clamp(base_w, base_w + 144), th.clamp(base_h, base_h + 48))
    } else {
        (base_w, base_h)
    };
    // centre the pod grid horizontally (left of the CEO corner, when reserved)
    let grid_w = cols as i32 * POD_W + (cols as i32 - 1) * GAP_X;
    let ceo = ex.ceo.then(|| [w - MARGIN - CEO_W, WALL_H + DECOR_H, CEO_W, CEO_H]);
    let x0 = match ceo {
        Some(c) => MARGIN + (c[0] - GAP_X - MARGIN - grid_w).max(0) / 2,
        None => (w - grid_w) / 2,
    };
    // centre the grid vertically in any spare height between the decor strip and the corridor
    let grid_h = rows as i32 * (POD_H + ZONE_HEAD + ZONE_PAD) + (rows as i32 - 1) * GAP_Y;
    let spare = (h - BOTTOM_H - (WALL_H + DECOR_H) - grid_h).max(0);
    let y0 = WALL_H + DECOR_H + ZONE_HEAD + spare / 2;
    let mut pods = Vec::new();
    let mut seats = HashMap::new();
    for (i, (kind, ids, group)) in pod_plans.into_iter().enumerate() {
        let (c, r) = ((i % cols) as i32, (i / cols) as i32);
        let mut pod = Pod {
            kind,
            x: x0 + c * (POD_W + GAP_X),
            y: y0 + r * (POD_H + ZONE_HEAD + ZONE_PAD + GAP_Y),
            desks: Default::default(),
            group,
        };
        for (d, id) in ids.into_iter().enumerate() {
            let (x, y) = pod.cell(d);
            seats.insert(id.clone(), Seat { pod: i, desk: d, x, y });
            pod.desks[d] = Some(id);
        }
        pods.push(pod);
    }
    // floor zones: merge runs of same-group pods in the same row
    let mut zones: Vec<Zone> = Vec::new();
    let mut seen_groups = std::collections::HashSet::new();
    for p in &pods {
        let rect = [p.x - ZONE_PAD, p.y - ZONE_HEAD, POD_W + ZONE_PAD * 2, POD_H + ZONE_HEAD + ZONE_PAD];
        if let Some(z) = zones.last_mut() {
            if z.group == p.group && z.rect[1] == rect[1] && z.rect[0] + z.rect[2] + GAP_X - ZONE_PAD * 2 == rect[0] {
                z.rect[2] = rect[0] + rect[2] - z.rect[0];
                continue;
            }
        }
        let head = seen_groups.insert(p.group);
        zones.push(Zone { group: p.group, session: group_hosts.get(p.group).cloned().flatten(), rect, head });
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
        if pod.kind == PodKind::Council {
            continue; // no desks to sit on in the council cabin
        }
        for d in 0..DESKS_PER_POD {
            let (ox, oy) = pod.cell(d);
            let (x, y) = desk_top(ox, oy);
            spots.push(Spot { kind: SpotKind::Desk(pi, d), x, y });
        }
    }
    Layout {
        w,
        h,
        cols,
        rows,
        pods,
        zones,
        seats,
        door: (MARGIN + 9, h - 14),
        lobby_bench: (MARGIN + 52, h - 22),
        coffee_bar: (MARGIN + 52, WALL_H + DECOR_H + 2),
        ceo,
        spots,
    }
}

/// Desk furniture rect [x, y, w, h] of the desk cell at (ox, oy) (as drawn by the renderer).
pub fn desk_rect(ox: i32, oy: i32) -> [i32; 4] {
    [ox + 4, oy + 32, 40, 16]
}

/// Floor aisle y below the desk of cell row `oy` (between the desk and the next row's chairs).
const fn aisle_below(oy: i32) -> i32 {
    oy + 56
}

/// Something the cat must not walk across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Obstacle {
    /// The spot that sits on top of it (the cat may hop onto it when that is its target), or
    /// `None` when it is never walkable (the CEO nameplate).
    pub spot: Option<SpotKind>,
    pub rect: [i32; 4],
}

impl Obstacle {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        let [rx, ry, rw, rh] = self.rect;
        x >= rx && x < rx + rw && y >= ry && y < ry + rh
    }
}

impl Layout {
    /// The corridor along the wall, in front of the top row (above every desk).
    pub fn aisle_top(&self) -> i32 {
        WALL_H + DECOR_H + 2
    }

    /// The CEO desk [x, y, w, h] (surface, front and legs).
    pub fn ceo_desk_rect(&self) -> Option<[i32; 4]> {
        self.ceo.map(|[cx, cy, cw, _]| [cx + 2, cy + CEO_DESK_TOP, cw - 4, 17])
    }

    /// The widest the CEO nameplate can be (it is centred on the desk front).
    pub fn nameplate_rect(&self) -> Option<[i32; 4]> {
        self.ceo.map(|[cx, cy, cw, _]| [cx + 6, cy + CEO_DESK_TOP + 6, cw - 12, 5 + 4])
    }

    /// Desks (every pod cell has one), the CEO desk and its nameplate.
    pub fn obstacles(&self) -> Vec<Obstacle> {
        let mut v = Vec::new();
        for (pi, pod) in self.pods.iter().enumerate() {
            if pod.kind == PodKind::Council {
                // the table (the cat may hop onto either half from the far-side seats' spots)
                let [tx, ty, tw, th] = COUNCIL_TABLE;
                v.push(Obstacle { spot: Some(SpotKind::Desk(pi, 0)), rect: [pod.x + tx, pod.y + ty, tw / 2, th] });
                v.push(Obstacle { spot: Some(SpotKind::Desk(pi, 1)), rect: [pod.x + tx + tw / 2, pod.y + ty, tw - tw / 2, th] });
                continue;
            }
            for d in 0..DESKS_PER_POD {
                let (ox, oy) = pod.cell(d);
                v.push(Obstacle { spot: Some(SpotKind::Desk(pi, d)), rect: desk_rect(ox, oy) });
            }
            // the bench joint + frosted privacy panel between the two seats of each row
            for r in 0..2 {
                let (ox, oy) = pod.cell(r * 2);
                v.push(Obstacle { spot: None, rect: [ox + CELL_W - 4, oy + 22, 8, 26] });
            }
        }
        if let Some(r) = self.ceo_desk_rect() {
            v.push(Obstacle { spot: Some(SpotKind::CeoDesk), rect: r });
        }
        if let Some(r) = self.nameplate_rect() {
            v.push(Obstacle { spot: None, rect: r });
        }
        v
    }

    /// The floor point the cat hops to/from for a spot (always on a horizontal aisle).
    /// Desks are reached from the aisle in front of them; the sofa, windowsill and CEO desk from
    /// the wall corridor (the CEO desk from behind, so the cat never crosses the nameplate).
    pub fn access(&self, s: &Spot) -> (i32, i32) {
        match s.kind {
            SpotKind::Desk(pi, _) => match self.pods.get(pi) {
                // council seats: reached from the floor in front of the cabin's door line
                Some(p) if p.kind == PodKind::Council => (s.x, p.y + POD_H - 8),
                _ => (s.x, aisle_below(s.y - 34)),
            },
            SpotKind::Floor => (s.x, s.y),
            SpotKind::Sofa | SpotKind::Sill | SpotKind::CeoDesk => (s.x, self.aisle_top()),
        }
    }

    /// Vertical aisles: left and right of each pod column (the privacy partitions close the gap
    /// between a pod's two desk columns), all left of the CEO corner.
    pub fn aisles_x(&self) -> Vec<i32> {
        let mut xs: Vec<i32> = Vec::new();
        for p in &self.pods {
            xs.extend([p.x - GAP_X / 2, p.x + POD_W + GAP_X / 2]);
        }
        if xs.is_empty() {
            xs.push(MARGIN / 2);
        }
        xs.sort_unstable();
        xs.dedup();
        xs
    }

    /// Wide vertical aisles between pod columns (for 16 px walking humans).
    pub fn outer_aisles_x(&self) -> Vec<i32> {
        let mut xs = Vec::new();
        for p in &self.pods {
            xs.push(p.x - GAP_X / 2);
            xs.push(p.x + POD_W + GAP_X / 2);
        }
        if xs.is_empty() {
            xs.push(MARGIN / 2 + 8);
        }
        xs.sort_unstable();
        xs.dedup();
        xs
    }

    /// Where a seat's owner enters/leaves: the aisle in front of the desk, or the council cabin's door.
    pub fn seat_aisle(&self, s: &Seat) -> (i32, i32) {
        match self.pods.get(s.pod) {
            Some(p) if p.kind == PodKind::Council => (p.x + POD_W / 2, p.y + POD_H + 1),
            _ => (s.x + 26, s.y + CELL_H - 3),
        }
    }

    /// Manhattan aisle route for walking agents (never cuts diagonally through desks).
    pub fn walk_route(&self, from: (i32, i32), to: (i32, i32)) -> Vec<(i32, i32)> {
        if (from.1 - to.1).abs() <= 4 {
            return vec![from, to];
        }
        let xv = self
            .outer_aisles_x()
            .into_iter()
            .min_by_key(|x| ((from.0 - x).abs() + (to.0 - x).abs(), *x))
            .unwrap_or(from.0);
        let mut pts = vec![from, (xv, from.1), (xv, to.1), to];
        pts.dedup();
        self.avoid_council_tables(pts)
    }

    /// Re-route any horizontal leg that would cut through a council cabin's table: step down to
    /// the floor in front of the table, cross the cabin there and step back up.
    fn avoid_council_tables(&self, mut pts: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
        if !self.pods.iter().any(|p| p.kind == PodKind::Council) {
            return pts;
        }
        let [_, ty, _, th] = COUNCIL_TABLE;
        for _ in 0..16 {
            let hit = pts.windows(2).enumerate().find_map(|(i, w)| {
                if w[0].1 != w[1].1 || w[0].0 == w[1].0 {
                    return None;
                }
                let y = w[0].1;
                let (lo, hi) = (w[0].0.min(w[1].0), w[0].0.max(w[1].0));
                self.pods
                    .iter()
                    .filter(|p| p.kind == PodKind::Council && y >= p.y + ty - 3 && y < p.y + ty + th + 3 && hi > p.x && lo < p.x + POD_W)
                    .map(|p| (i, p.y + ty + th + 5))
                    .next()
            });
            let Some((i, y2)) = hit else { break };
            let (a, b) = (pts[i], pts[i + 1]);
            pts.splice(i + 1..i + 1, [(a.0, y2), (b.0, y2)]);
            pts.dedup();
        }
        pts
    }

    /// Manhattan route between two spots along floor aisles: hop down to the access point, walk
    /// the aisle to a vertical aisle, along it, then the target's aisle, and hop up. Returns the
    /// polyline including both end points (consecutive duplicates removed).
    pub fn route(&self, from: &Spot, to: &Spot) -> Vec<(i32, i32)> {
        if from == to {
            return vec![(from.x, from.y)];
        }
        let a = self.access(from);
        let b = self.access(to);
        let mut pts = vec![(from.x, from.y), a];
        if a.1 != b.1 {
            let xv = self
                .aisles_x()
                .into_iter()
                .min_by_key(|x| ((a.0 - x).abs() + (b.0 - x).abs(), *x))
                .unwrap_or(a.0);
            pts.push((xv, a.1));
            pts.push((xv, b.1));
        }
        pts.push(b);
        pts.push((to.x, to.y));
        pts.dedup();
        self.avoid_council_tables(pts)
    }
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
            current_tool: None,
            last_tool: None,
            last_tool_ok: None,
            slot: None,
            pid: None,
            state: AgentState::Idle,
            activity: String::new(),
            started_ms: t,
            last_event_ms: t,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
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
        let off = build_with(&Scene::default(), 640, 360, Extras { ceo: false, council: true, ..Default::default() });
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

    #[test]
    fn each_session_gets_its_own_floor_zone_and_one_nameplate() {
        let mut s = Scene::default();
        for (n, id) in ["cc:a", "cc:b", "cc:c"].iter().enumerate() {
            s.agents.insert((*id).into(), agent(id, AgentKind::Claude, None, Some(&format!("/p/{n}")), n as u64));
        }
        // five subagents for cc:a -> it spills into an overflow pod in the same zone run
        for k in 0..5 {
            let id = format!("cc-sub:a/{k}");
            s.agents.insert(id.clone(), agent(&id, AgentKind::ClaudeSub, Some("cc:a"), None, 10 + k));
        }
        s.agents.insert("agy:1".into(), agent("agy:1", AgentKind::Agy, None, Some("/elsewhere"), 50));
        let l = build(&s, 1600, 600);
        let heads: Vec<_> = l.zones.iter().filter(|z| z.head).collect();
        assert_eq!(heads.len(), 4, "3 sessions + lab: {:?}", l.zones);
        assert_eq!(heads.iter().filter(|z| z.session.is_none()).count(), 1);
        for (i, a) in l.zones.iter().enumerate() {
            for b in &l.zones[i + 1..] {
                let [ax, ay, aw, ah] = a.rect;
                let [bx, by, bw, bh] = b.rect;
                assert!(ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay, "zones overlap: {a:?} {b:?}");
            }
        }
        // every pod sits inside a zone of its own group
        for p in &l.pods {
            assert!(l.zones.iter().any(|z| z.group == p.group
                && z.rect[0] <= p.x && p.x + POD_W <= z.rect[0] + z.rect[2]
                && z.rect[1] <= p.y && p.y + POD_H <= z.rect[1] + z.rect[3]));
        }
    }


    fn council_scene() -> Scene {
        let mut v = vec![
            agent("cc:a", AgentKind::Claude, None, Some("/p/a"), 1),
            agent("cc:b", AgentKind::Claude, None, Some("/p/b"), 2),
            agent("cc-sub:a/1", AgentKind::ClaudeSub, Some("cc:a"), None, 3),
        ];
        for (i, role) in ["agy-council-architect", "agy-council-advisor", "agy-council-advisor", "agy-council-advisor"].iter().enumerate() {
            let mut a = agent(&format!("cc-sub:a/c{i}"), AgentKind::ClaudeSub, Some("cc:a"), None, 10 + i as u64);
            a.role = Some((*role).into());
            v.push(a);
        }
        scene(v)
    }

    #[test]
    fn council_seats_sit_two_per_side_of_a_centred_table() {
        let l = build(&council_scene(), 1280, 720);
        let pod = l.pods.iter().find(|p| p.kind == PodKind::Council).expect("council pod");
        let [tx, ty, tw, th] = COUNCIL_TABLE;
        let cells: Vec<(i32, i32)> = (0..DESKS_PER_POD).map(|d| pod.cell(d)).collect();
        // far side above the table, near side below it, symmetric about the table centre
        assert!(cells[0].1 + 30 <= pod.y + ty + 1 && cells[1].1 == cells[0].1);
        assert!(cells[2].1 + 13 + 2 >= pod.y + ty + th && cells[3].1 == cells[2].1);
        let centre = pod.x + tx + tw / 2;
        assert_eq!((cells[0].0 + 26) + (cells[1].0 + 26), centre * 2);
        // every council agent has a seat in the cabin
        assert_eq!(pod.desks.iter().flatten().count(), 4);
    }

    #[test]
    fn walking_routes_avoid_the_council_table_and_desk_partitions() {
        let s = council_scene();
        for (pw, ph) in [(640, 360), (1280, 720), (1920, 1080)] {
            let l = build(&s, pw, ph);
            let obstacles = l.obstacles();
            let mut spots = l.spots.clone();
            for (pi, p) in l.pods.iter().enumerate() {
                if p.kind == PodKind::Council {
                    for d in 0..2 {
                        let (ox, oy) = p.cell(d);
                        let (x, y) = desk_top(ox, oy);
                        spots.push(Spot { kind: SpotKind::Desk(pi, d), x, y });
                    }
                }
            }
            for from in &spots {
                for to in &spots {
                    let pts = l.route(from, to);
                                        for (si, w) in pts.windows(2).enumerate() {
                        // the first and last legs are the deliberate hops off/onto a desk
                        if si == 0 || si + 1 == pts.len() - 1 {
                            continue;
                        }
                        let n = ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()).max(1);
                        for k in 0..=n {
                            let x = w[0].0 + (w[1].0 - w[0].0) * k / n;
                            let y = w[0].1 + (w[1].1 - w[0].1) * k / n;
                            assert!(!obstacles.iter().any(|o| o.contains(x, y)), "route {pts:?} crosses an obstacle at {:?}", (x, y));
                        }
                    }
                }
            }
        }
    }
}
