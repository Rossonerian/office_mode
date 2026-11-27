//! Data behind the sidebar tabs (Jobs, Activity, Stats): derived from the scene, pure and tested.
use super::fmt::state_color;
use super::reader::{self, Verdict};
use office_core::{AgentId, AgentState, AlertLevel, AutoJob, Report, ReportKind, Scene};
use ratatui::style::Color;
use std::collections::{BTreeMap, VecDeque};

/// The sidebar's tabs (keys 1-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SideTab {
    #[default]
    Agents,
    Jobs,
    Activity,
    Stats,
}

impl SideTab {
    pub const ALL: [SideTab; 4] = [SideTab::Agents, SideTab::Jobs, SideTab::Activity, SideTab::Stats];

    pub fn title(self) -> &'static str {
        match self {
            SideTab::Agents => "1 Agents",
            SideTab::Jobs => "2 Jobs",
            SideTab::Activity => "3 Activity",
            SideTab::Stats => "4 Stats",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// Tab for a digit key.
    pub fn from_digit(c: char) -> Option<SideTab> {
        c.to_digit(10).and_then(|d| (d as usize).checked_sub(1)).and_then(|i| Self::ALL.get(i).copied())
    }

    /// Which tab a click at column offset `dx` (from the bar's left edge) lands on.
    pub fn at_offset(dx: u16) -> Option<SideTab> {
        let mut x = 0u16;
        for t in Self::ALL {
            let w = t.title().chars().count() as u16;
            if dx >= x && dx < x + w {
                return Some(t);
            }
            x += w + 1;
        }
        None
    }
}

// ---------- jobs ----------

/// The ROSSO pipeline, in order: a job's progress is the furthest stage a document reached.
pub const STAGES: [&str; 7] = ["memo", "task card", "critique", "decision", "work order", "sign-off", "inspection"];

/// Pipeline stage (index into `STAGES`) a document type belongs to.
pub fn stage_of(doc_type: &str) -> Option<usize> {
    let t = doc_type.to_ascii_lowercase();
    let has = |names: &[&str]| names.iter().any(|n| t.contains(n));
    if has(&["task card", "amm", "proposal", "revision", "plan"]) {
        Some(1)
    } else if has(&["work order"]) {
        Some(4)
    } else if has(&["sign-off", "signoff", "sign off", "approval"]) {
        Some(5)
    } else if has(&["inspection", "test report", "verdict", "test"]) {
        Some(6)
    } else if has(&["critique", "review", "rebuttal", "re-check", "recheck"]) {
        Some(2)
    } else if has(&["decision", "ruling", "ledger", "arbitrat"]) {
        Some(3)
    } else if has(&["memo"]) {
        Some(0)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Active,
    Idle,
    Done,
    Blocked,
    Failed,
}

impl JobStatus {
    pub fn label(self) -> &'static str {
        match self {
            JobStatus::Active => "active",
            JobStatus::Idle => "idle",
            JobStatus::Done => "done",
            JobStatus::Blocked => "blocked",
            JobStatus::Failed => "failed",
        }
    }

    pub fn color(self) -> Color {
        match self {
            JobStatus::Active => Color::Cyan,
            JobStatus::Idle => Color::DarkGray,
            JobStatus::Done => Color::Green,
            JobStatus::Blocked => Color::Yellow,
            JobStatus::Failed => Color::LightRed,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub name: String,
    pub title: String,
    pub flow: String,
    pub status: JobStatus,
    pub docs: usize,
    pub verdict: Option<Verdict>,
    pub last_ms: u64,
    /// Furthest pipeline stage reached (index into `STAGES`).
    pub stage: Option<usize>,
    pub ratio: f64,
    /// Report id of the newest document (what Enter opens).
    pub last_report: u64,
    pub wave: Option<office_core::WaveState>,
    pub auto: Option<AutoJob>,
}

fn auto_status(aj: &AutoJob) -> JobStatus {
    match aj.status.trim().to_ascii_lowercase().as_str() {
        "running" | "active" => JobStatus::Active,
        "done" | "completed" | "complete" => JobStatus::Done,
        "needs_input" | "needs-input" | "blocked" | "paused" | "waiting" => JobStatus::Blocked,
        "failed" | "error" | "aborted" | "timeout" | "timed_out" => JobStatus::Failed,
        _ if aj.needs_path.is_some() => JobStatus::Blocked,
        _ if aj.last_error.is_some() => JobStatus::Failed,
        _ => JobStatus::Idle,
    }
}

fn auto_ratio(aj: &AutoJob, status: JobStatus) -> f64 {
    if status == JobStatus::Done {
        return 1.0;
    }
    let (step, total) = office_core::rosso::auto_phase_progress(&aj.flow, &aj.phase);
    if total > 0 {
        step as f64 / total as f64
    } else {
        0.0
    }
}

/// One row per ROSSO job seen in the scene's hand-offs, waves, or `rosso-auto` jobs, most recently active first.
pub fn jobs(scene: &Scene) -> Vec<JobRow> {
    let mut by_job: BTreeMap<&str, Vec<&Report>> = BTreeMap::new();
    for r in &scene.reports {
        if let Some(j) = scene.report_jobs.get(&r.id) {
            by_job.entry(j.as_str()).or_default().push(r);
        }
    }
    let mut auto_by_name: BTreeMap<&str, &AutoJob> = BTreeMap::new();
    for aj in &scene.auto_jobs {
        auto_by_name
            .entry(aj.job.as_str())
            .and_modify(|cur| {
                if (aj.status == "running", aj.started_at) > (cur.status == "running", cur.started_at) {
                    *cur = aj;
                }
            })
            .or_insert(aj);
    }
    let mut rows: Vec<JobRow> = Vec::new();
    for (name, reps) in &by_job {
        // One document can be delivered to several recipients: count it once.
        let mut docs: Vec<&Report> = Vec::new();
        for &r in reps {
            if !docs.iter().any(|d| d.title == r.title && d.from_label == r.from_label && d.at_ms == r.at_ms) {
                docs.push(r);
            }
        }
        let (Some(first), Some(last)) = (docs.first(), docs.last()) else { continue };
        let stage = docs.iter().filter_map(|d| reader::doc_type(&d.title).and_then(|t| stage_of(&t))).max();
        let verdict_of = |d: &Report| reader::detect_verdict(&d.title, &reader::parse_body(&d.body));
        let verdict = docs.iter().rev().find_map(|d| verdict_of(d));
        let last_v = verdict_of(last);
        let escalated = last.title.starts_with('\u{26a0}');
        let blocked = escalated || matches!(last_v, Some(Verdict::Blocked | Verdict::NeedsUserInput));
        let status = if blocked {
            JobStatus::Blocked
        } else if last_v.is_some_and(Verdict::bad) {
            JobStatus::Failed
        } else if stage == Some(STAGES.len() - 1) && last_v.is_some_and(Verdict::good) {
            JobStatus::Done
        } else if scene.now_ms.saturating_sub(last.at_ms) < 600_000 {
            JobStatus::Active
        } else {
            JobStatus::Idle
        };
        let subject = reader::subject_of(&first.title);
        rows.push(JobRow {
            name: (*name).to_string(),
            title: if subject.is_empty() { first.title.clone() } else { subject },
            flow: format!("{} → {}", last.from_label, last.to_label),
            status,
            docs: docs.len(),
            verdict,
            last_ms: last.at_ms,
            stage,
            ratio: stage.map(|s| (s + 1) as f64 / STAGES.len() as f64).unwrap_or(0.0),
            last_report: last.id,
            wave: scene.waves.get(*name).cloned(),
            auto: None,
        });
    }
    for (name, w) in &scene.waves {
        if by_job.contains_key(name.as_str()) {
            continue;
        }
        let tot = w.done + w.relay_inflight + w.pending;
        rows.push(JobRow {
            name: name.clone(),
            title: format!("wave {}", w.plan),
            flow: format!("{} done · {} relay", w.done, w.relay_inflight),
            status: if w.relay_inflight > 0 || w.pending > 0 { JobStatus::Active } else { JobStatus::Done },
            docs: w.done as usize,
            verdict: w.calls.iter().rev().find_map(|c| {
                c.verdict.as_deref().and_then(|s| Verdict::from_word(&s.trim().to_ascii_uppercase(), false))
            }),
            last_ms: w.updated_ms,
            stage: None,
            ratio: if tot > 0 { w.done as f64 / tot as f64 } else { 0.0 },
            last_report: 0,
            wave: Some(w.clone()),
            auto: None,
        });
    }
    for row in &mut rows {
        if let Some(&aj) = auto_by_name.get(row.name.as_str()) {
            let a_status = auto_status(aj);
            if a_status != JobStatus::Idle {
                row.status = a_status;
            }
            row.ratio = row.ratio.max(auto_ratio(aj, a_status));
            if row.verdict.is_none() {
                row.verdict = aj
                    .ruling
                    .as_deref()
                    .and_then(|s| Verdict::from_word(&s.trim().to_ascii_uppercase(), false));
            }
            row.last_ms = row.last_ms.max(aj.started_at);
            row.auto = Some(aj.clone());
        }
    }
    for (name, aj) in auto_by_name {
        if rows.iter().any(|r| r.name == name) {
            continue;
        }
        let status = auto_status(aj);
        let (step, total) = office_core::rosso::auto_phase_progress(&aj.flow, &aj.phase);
        let title = if aj.task.trim().is_empty() {
            format!("auto {}", aj.flow)
        } else {
            aj.task.clone()
        };
        let flow = if total > 0 {
            format!("auto {} · {} ({}/{})", aj.flow, aj.phase, step, total)
        } else if !aj.flow.is_empty() {
            format!("auto {} · {}", aj.flow, aj.phase)
        } else {
            format!("auto {}", aj.phase)
        };
        rows.push(JobRow {
            name: aj.job.clone(),
            title,
            flow,
            status,
            docs: 0,
            verdict: aj
                .ruling
                .as_deref()
                .and_then(|s| Verdict::from_word(&s.trim().to_ascii_uppercase(), false)),
            last_ms: aj.started_at,
            stage: None,
            ratio: auto_ratio(aj, status),
            last_report: 0,
            wave: None,
            auto: Some(aj.clone()),
        });
    }
    rows.sort_by(|a, b| b.last_ms.cmp(&a.last_ms).then_with(|| a.name.cmp(&b.name)));
    rows
}

// ---------- activity feed ----------

/// A notable state change of an agent (kept by the app, bounded).
#[derive(Debug, Clone, PartialEq)]
pub struct StateNote {
    pub at_ms: u64,
    pub label: String,
    pub state: AgentState,
}

pub const MAX_STATE_NOTES: usize = 40;
pub const FEED_MAX: usize = 60;

/// States worth a line in the feed.
pub fn notable(s: AgentState) -> bool {
    matches!(s, AgentState::NeedsYou | AgentState::Error | AgentState::Done | AgentState::Queued)
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedItem {
    pub at_ms: u64,
    pub icon: &'static str,
    pub color: Color,
    pub route: String,
    pub text: String,
    pub report: Option<u64>,
}

/// Icon + colour for a hand-off: ✉ brief, 📁 report, ⬆ result, ⚠ escalation, ✓/✗ for a verdict.
pub fn report_icon(r: &Report) -> (&'static str, Color) {
    if r.title.starts_with('\u{26a0}') {
        return ("⚠", Color::LightRed);
    }
    if r.kind == ReportKind::Task {
        return ("✉", Color::Gray);
    }
    match reader::detect_verdict(&r.title, &reader::parse_body(&r.body)) {
        Some(v) if v.good() => ("✓", Color::Green),
        Some(v) if v.bad() => ("✗", Color::LightRed),
        _ if r.kind == ReportKind::Report => ("📁", Color::Yellow),
        _ => ("⬆", Color::LightBlue),
    }
}

/// The newest hand-offs and state changes, newest first.
pub fn feed(scene: &Scene, notes: &VecDeque<StateNote>, limit: usize) -> Vec<FeedItem> {
    let mut v: Vec<FeedItem> = scene
        .reports
        .iter()
        .rev()
        .take(limit)
        .map(|r| {
            let (icon, color) = report_icon(r);
            FeedItem {
                at_ms: r.at_ms,
                icon,
                color,
                route: format!("{} → {}", r.from_label, r.to_label),
                text: r.title.clone(),
                report: Some(r.id),
            }
        })
        .collect();
    v.extend(notes.iter().rev().take(limit).map(|n| FeedItem {
        at_ms: n.at_ms,
        icon: "●",
        color: state_color(n.state),
        route: n.label.clone(),
        text: match n.state {
            AgentState::NeedsYou => "needs you".into(),
            AgentState::Error => "hit an error".into(),
            AgentState::Done => "finished".into(),
            AgentState::Queued => "queued for a slot".into(),
            s => format!("{s:?}").to_lowercase(),
        },
        report: None,
    }));
    v.sort_by_key(|a| std::cmp::Reverse(a.at_ms));
    v.truncate(limit);
    v
}

// ---------- stats ----------

/// Agents doing something right now.
pub fn is_active(s: AgentState) -> bool {
    matches!(s, AgentState::Thinking | AgentState::Typing | AgentState::Reading | AgentState::Running | AgentState::Delegating)
}

pub fn active_count(scene: &Scene) -> usize {
    scene.agents.values().filter(|a| is_active(a.state)).count()
}

/// (fresh + cache-write input tokens, output tokens) over all agents.
pub fn token_totals(scene: &Scene) -> (u64, u64) {
    scene.agents.values().fold((0, 0), |(i, o), a| (i + a.tokens_in + a.tokens_cache_write, o + a.tokens_out))
}

/// Cost per session (a root agent plus everything under it), most expensive first, at most `n`.
pub fn session_costs(scene: &Scene, n: usize) -> Vec<(String, f64)> {
    let root_of = |id: &AgentId| -> AgentId {
        let mut cur = id.clone();
        for _ in 0..16 {
            match scene.agents.get(&cur).and_then(|a| a.parent.clone()).filter(|p| scene.agents.contains_key(p)) {
                Some(p) => cur = p,
                None => break,
            }
        }
        cur
    };
    let mut by_root: BTreeMap<AgentId, f64> = BTreeMap::new();
    for a in scene.agents.values() {
        *by_root.entry(root_of(&a.id)).or_default() += a.cost_usd;
    }
    let mut v: Vec<(String, f64)> = by_root
        .into_iter()
        .map(|(id, c)| (scene.agents.get(&id).map(|a| a.label.clone()).unwrap_or(id), c))
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

/// (info, warn, critical) alert counts.
pub fn alert_counts(scene: &Scene) -> (usize, usize, usize) {
    let c = |l: AlertLevel| scene.alerts.iter().filter(|a| a.level == l).count();
    (c(AlertLevel::Info), c(AlertLevel::Warn), c(AlertLevel::Critical))
}

/// RAM headroom gauge data `(label, ratio)` when router reports `free_mb` and `per_proc_mb`.
/// Ratio represents available capacity headroom relative to `max_concurrent` (or 4 slots fallback).
pub fn ram_headroom_ratio(scene: &Scene) -> Option<(String, f64)> {
    let free = scene.router.free_mb?;
    let per = scene.router.per_proc_mb.filter(|&p| p > 0)?;
    let slots = scene.router.max_concurrent.or(scene.router.capacity).unwrap_or(4).max(1);
    let target_mb = (per as f64) * (slots as f64);
    let ratio = (free as f64 / target_mb).clamp(0.0, 1.0);
    let label = format!("RAM free {free} MB ({:.0}%)", ratio * 100.0);
    Some((label, ratio))
}

/// Sample spacing and history length: one point per 10 s, 30 minutes.
pub const SAMPLE_EVERY_MS: u64 = 10_000;
pub const SAMPLE_CAP: usize = 180;

/// Bounded history of total cost, active-agent count, and total tokens.
#[derive(Debug, Clone, Default)]
pub struct Samples {
    pub cost: VecDeque<f64>,
    pub active: VecDeque<u64>,
    pub tokens: VecDeque<u64>,
    last_ms: Option<u64>,
}

impl Samples {
    /// Record a sample if 10 s have passed since the last one. Returns true when one was taken.
    pub fn sample(&mut self, scene: &Scene, now_ms: u64) -> bool {
        if self.last_ms.is_some_and(|l| now_ms < l + SAMPLE_EVERY_MS) {
            return false;
        }
        self.last_ms = Some(now_ms);
        self.cost.push_back(scene.total_cost_usd);
        self.active.push_back(active_count(scene) as u64);
        self.tokens.push_back(scene.total_tokens);
        while self.cost.len() > SAMPLE_CAP {
            self.cost.pop_front();
        }
        while self.active.len() > SAMPLE_CAP {
            self.active.pop_front();
        }
        while self.tokens.len() > SAMPLE_CAP {
            self.tokens.pop_front();
        }
        true
    }

    /// The last `width` cost points, rebased to the lowest so growth shows (hundredths of a cent).
    pub fn cost_series(&self, width: usize) -> Vec<u64> {
        let skip = self.cost.len().saturating_sub(width);
        let tail: Vec<f64> = self.cost.iter().skip(skip).copied().collect();
        let min = tail.iter().copied().fold(f64::INFINITY, f64::min);
        tail.iter().map(|v| ((v - min).max(0.0) * 10_000.0).round() as u64).collect()
    }

    pub fn active_series(&self, width: usize) -> Vec<u64> {
        let skip = self.active.len().saturating_sub(width);
        self.active.iter().skip(skip).copied().collect()
    }

    /// The last `width` token points, rebased to the window minimum so growth is visible.
    pub fn token_series(&self, width: usize) -> Vec<u64> {
        let skip = self.tokens.len().saturating_sub(width);
        let tail: Vec<u64> = self.tokens.iter().skip(skip).copied().collect();
        let min = tail.iter().copied().min().unwrap_or(0);
        tail.iter().map(|v| v.saturating_sub(min)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    fn rep(id: u64, at: u64, kind: ReportKind, from: &str, to: &str, title: &str, body: &str) -> Report {
        Report {
            id,
            at_ms: at,
            kind,
            from: format!("rosso:{from}"),
            from_label: from.into(),
            to: Some(format!("rosso:{to}")),
            to_label: to.into(),
            title: title.into(),
            body: body.into(),
            brief: String::new(),
        }
    }

    fn job_scene() -> Scene {
        let mut s = demo_scene();
        s.now_ms = 100_000;
        let docs = [
            (1, 10_000, ReportKind::Task, "Chief of Staff", "Chief Architect", "Memo: Add login", "do it", "j-login"),
            (2, 20_000, ReportKind::Report, "Chief Architect", "Chief of Staff", "Task Card: Add login", "cards", "j-login"),
            (3, 30_000, ReportKind::Report, "Review Seat", "Chief of Staff", "Critique: Add login [REVISE]", "x", "j-login"),
            (4, 40_000, ReportKind::Task, "Chief of Staff", "Backend Head", "Memo: Fix cache", "do", "j-cache"),
            (5, 90_000, ReportKind::Report, "Test Inspector", "Chief of Staff", "Inspection: Fix cache [PASS]", "ok", "j-cache"),
        ];
        for (id, at, k, f, t, title, body, job) in docs {
            s.reports.push(rep(id, at, k, f, t, title, body));
            s.report_jobs.insert(id, job.into());
        }
        // A delivery to two recipients counts as one document.
        s.reports.push(rep(6, 95_000, ReportKind::Report, "Technician", "Chief of Staff", "Work Order: Fix cache", "w"));
        s.reports.push(rep(7, 95_000, ReportKind::Report, "Technician", "Backend Head", "Work Order: Fix cache", "w"));
        s.report_jobs.insert(6, "j-cache".into());
        s.report_jobs.insert(7, "j-cache".into());
        // A hand-off without a job is not a job.
        s.reports.push(rep(8, 96_000, ReportKind::Report, "Explore", "cc", "Findings", "f"));
        s
    }

    #[test]
    fn jobs_are_derived_from_reports() {
        let js = jobs(&job_scene());
        assert_eq!(js.len(), 2);
        let cache = &js[0];
        assert_eq!(cache.name, "j-cache");
        assert_eq!(cache.title, "Fix cache");
        assert_eq!(cache.docs, 3, "{cache:?}");
        assert_eq!(cache.stage, Some(6));
        assert_eq!(cache.ratio, 1.0);
        assert_eq!(cache.verdict, Some(Verdict::Pass));
        assert_eq!(cache.last_ms, 95_000);
        assert_eq!(cache.status, JobStatus::Active, "last doc is a work order: not signed off yet");
        let login = &js[1];
        assert_eq!(login.name, "j-login");
        assert_eq!(login.docs, 3);
        assert_eq!(login.stage, Some(2));
        assert!((login.ratio - 3.0 / 7.0).abs() < 1e-9);
        assert_eq!(login.verdict, Some(Verdict::Revise));
        assert_eq!(login.flow, "Review Seat → Chief of Staff");
        assert_eq!(login.status, JobStatus::Active, "70 s old");
        let mut later = job_scene();
        later.now_ms = 2_000_000;
        assert_eq!(jobs(&later)[1].status, JobStatus::Idle);
    }

    #[test]
    fn job_status_done_failed_blocked() {
        let mut s = job_scene();
        s.reports.push(rep(9, 99_000, ReportKind::Report, "Test Inspector", "Chief of Staff", "Inspection: Fix cache [PASS]", "ok"));
        s.report_jobs.insert(9, "j-cache".into());
        assert_eq!(jobs(&s)[0].status, JobStatus::Done);
        s.reports.push(rep(10, 99_500, ReportKind::Report, "Test Inspector", "Chief of Staff", "Inspection: Fix cache [FAIL]", "no"));
        s.report_jobs.insert(10, "j-cache".into());
        assert_eq!(jobs(&s)[0].status, JobStatus::Failed);
        s.reports.push(rep(11, 99_900, ReportKind::Result, "Chief of Staff", "CEO", "\u{26a0} Escalation: R-1", "?"));
        s.report_jobs.insert(11, "j-cache".into());
        assert_eq!(jobs(&s)[0].status, JobStatus::Blocked);
    }

    #[test]
    fn jobs_attach_wave_and_synthesize_wave_only_jobs() {
        use office_core::{WaveCall, WaveState};
        let mut s = job_scene();
        s.waves.insert(
            "j-login".into(),
            WaveState {
                job: "j-login".into(),
                plan: "council-r2".into(),
                updated_ms: 80_000,
                done: 1,
                relay_inflight: 1,
                pending: 2,
                calls: vec![WaveCall { title: "Security Seat".into(), status: "OK".into(), verdict: Some("APPROVE".into()), relay: false }],
            },
        );
        s.waves.insert(
            "j-wave-only".into(),
            WaveState {
                job: "j-wave-only".into(),
                plan: "inspect".into(),
                updated_ms: 98_000,
                done: 2,
                relay_inflight: 1,
                pending: 1,
                calls: vec![
                    WaveCall { title: "Test Inspector".into(), status: "OK".into(), verdict: Some("PASS".into()), relay: false },
                    WaveCall { title: "Chief Inspector".into(), status: "RELAY".into(), verdict: None, relay: true },
                ],
            },
        );
        s.waves.insert(
            "j-wave-done".into(),
            WaveState {
                job: "j-wave-done".into(),
                plan: "council-r1".into(),
                updated_ms: 15_000,
                done: 3,
                relay_inflight: 0,
                pending: 0,
                calls: vec![WaveCall { title: "Review Seat".into(), status: "OK".into(), verdict: Some("REVISE".into()), relay: false }],
            },
        );

        let js = jobs(&s);
        assert_eq!(js.len(), 4);
        let wave_only = &js[0];
        assert_eq!(wave_only.name, "j-wave-only");
        assert_eq!(wave_only.title, "wave inspect");
        assert_eq!(wave_only.flow, "2 done · 1 relay");
        assert_eq!(wave_only.status, JobStatus::Active);
        assert_eq!(wave_only.docs, 2);
        assert_eq!(wave_only.verdict, Some(Verdict::Pass));
        assert_eq!(wave_only.last_ms, 98_000);
        assert_eq!(wave_only.stage, None);
        assert!((wave_only.ratio - 0.5).abs() < 1e-9);
        assert_eq!(wave_only.last_report, 0);
        assert_eq!(wave_only.wave.as_ref().map(|w| w.plan.as_str()), Some("inspect"));

        let cache = js.iter().find(|j| j.name == "j-cache").unwrap();
        assert_eq!(cache.wave, None);
        let login = js.iter().find(|j| j.name == "j-login").unwrap();
        assert_eq!(login.wave.as_ref().map(|w| w.plan.as_str()), Some("council-r2"));
        let wave_done = js.iter().find(|j| j.name == "j-wave-done").unwrap();
        assert_eq!(wave_done.status, JobStatus::Done);
        assert_eq!(wave_done.verdict, Some(Verdict::Revise));
        assert!((wave_done.ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jobs_attach_auto_and_synthesize_auto_only_jobs() {
        let mut s = job_scene();
        // Make j-login appear idle by age, then attach an active AutoJob at phase execute (6/8 = 0.75).
        s.now_ms = 2_000_000;
        s.auto_jobs.push(AutoJob {
            job: "j-login".into(),
            task: "Add login".into(),
            flow: "council".into(),
            phase: "execute".into(),
            status: "running".into(),
            calls_used: 4,
            max_calls: 12,
            started_at: 96_000,
            ..Default::default()
        });
        // Synthesize an auto-only job in the simple flow at phase design (2/5 = 0.4).
        s.auto_jobs.push(AutoJob {
            job: "j-auto-only".into(),
            task: "Refactor auth".into(),
            flow: "simple".into(),
            phase: "design".into(),
            status: "done".into(),
            calls_used: 3,
            max_calls: 6,
            started_at: 120_000,
            ruling: Some("APPROVE".into()),
            ..Default::default()
        });

        let js = jobs(&s);
        assert_eq!(js.len(), 3);

        let auto_only = &js[0];
        assert_eq!(auto_only.name, "j-auto-only");
        assert_eq!(auto_only.title, "Refactor auth");
        assert_eq!(auto_only.flow, "auto simple · design (2/5)");
        assert_eq!(auto_only.status, JobStatus::Done);
        assert_eq!(auto_only.verdict, Some(Verdict::Approve));
        assert!((auto_only.ratio - 1.0).abs() < 1e-9);
        assert_eq!(auto_only.last_report, 0);
        assert_eq!(auto_only.auto.as_ref().map(|a| a.calls_used), Some(3));

        let login = js.iter().find(|j| j.name == "j-login").unwrap();
        assert_eq!(login.status, JobStatus::Active);
        assert!((login.ratio - 0.75).abs() < 1e-9);
        assert_eq!(login.last_ms, 96_000);
        assert_eq!(login.auto.as_ref().map(|a| a.phase.as_str()), Some("execute"));

        let cache = js.iter().find(|j| j.name == "j-cache").unwrap();
        assert_eq!(cache.auto, None);
    }

    #[test]
    fn stages_map_doc_types() {
        assert_eq!(stage_of("Memo"), Some(0));
        assert_eq!(stage_of("Task Card"), Some(1));
        assert_eq!(stage_of("Critique"), Some(2));
        assert_eq!(stage_of("Decision"), Some(3));
        assert_eq!(stage_of("Work Order"), Some(4));
        assert_eq!(stage_of("Sign-off"), Some(5));
        assert_eq!(stage_of("Inspection"), Some(6));
        assert_eq!(stage_of("Escalation"), None);
    }

    #[test]
    fn feed_merges_reports_and_state_changes_newest_first() {
        let s = job_scene();
        let mut notes = VecDeque::new();
        notes.push_back(StateNote { at_ms: 97_000, label: "cc·office".into(), state: AgentState::NeedsYou });
        let f = feed(&s, &notes, 50);
        assert_eq!(f[0].text, "needs you");
        assert_eq!(f[0].report, None);
        assert_eq!(f[1].report, Some(8));
        assert_eq!(report_icon(&s.reports[0]).0, "✉");
        assert_eq!(report_icon(&s.reports[1]).0, "📁");
        assert_eq!(report_icon(&s.reports[4]).0, "✓");
        let mut esc = s.reports[0].clone();
        esc.title = "\u{26a0} Escalation: x".into();
        assert_eq!(report_icon(&esc).0, "⚠");
        esc.title = "x".into();
        esc.kind = ReportKind::Result;
        assert_eq!(report_icon(&esc).0, "⬆");
        esc.title = "Inspection: y [FAIL]".into();
        assert_eq!(report_icon(&esc).0, "✗");
        assert_eq!(feed(&s, &notes, 3).len(), 3);
    }

    #[test]
    fn samples_are_spaced_and_bounded() {
        let mut s = demo_scene();
        let mut sm = Samples::default();
        assert!(sm.sample(&s, 1_000));
        assert!(!sm.sample(&s, 5_000), "10 s spacing");
        assert!(sm.sample(&s, 11_000));
        for i in 0..(SAMPLE_CAP as u64 * 2) {
            s.total_cost_usd = i as f64 * 0.01;
            sm.sample(&s, 20_000 + i * SAMPLE_EVERY_MS);
        }
        assert_eq!(sm.cost.len(), SAMPLE_CAP);
        assert_eq!(sm.active.len(), SAMPLE_CAP);
        assert_eq!(sm.tokens.len(), SAMPLE_CAP);
        assert_eq!(sm.cost_series(10).len(), 10);
        assert_eq!(sm.cost_series(10)[0], 0, "rebased to the window minimum");
        assert!(sm.cost_series(10)[9] > 0);
        assert_eq!(sm.active_series(500).len(), SAMPLE_CAP);
        assert_eq!(sm.token_series(10).len(), 10);
    }

    #[test]
    fn token_series_and_ram_headroom() {
        let mut s = demo_scene();
        let (lbl, ratio) = ram_headroom_ratio(&s).unwrap();
        assert!(lbl.contains("RAM free 1800 MB"), "{lbl}");
        assert!((ratio - 1.0).abs() < 1e-9, "{ratio}");

        s.router.free_mb = Some(700);
        let (lbl2, ratio2) = ram_headroom_ratio(&s).unwrap();
        assert!(lbl2.contains("700 MB") && lbl2.contains("50%"), "{lbl2}");
        assert!((ratio2 - 0.5).abs() < 1e-9, "{ratio2}");

        let mut sm = Samples::default();
        s.total_tokens = 1_000;
        sm.sample(&s, 1_000);
        s.total_tokens = 2_500;
        sm.sample(&s, 11_000);
        assert_eq!(sm.token_series(5), vec![0, 1_500]);
    }

    #[test]
    fn stats_helpers() {
        let s = demo_scene();
        assert_eq!(active_count(&s), 3, "reading, thinking, typing");
        assert_eq!(token_totals(&s), (14_100, 1_000));
        assert_eq!(alert_counts(&s), (1, 1, 0));
        let sc = session_costs(&s, 6);
        assert!(sc.iter().any(|(l, c)| l == "cc·DICE" && (*c - 1.0).abs() < 1e-9), "{sc:?}");
        assert_eq!(session_costs(&s, 1).len(), 1);
    }

    #[test]
    fn side_tabs_keys_and_clicks() {
        assert_eq!(SideTab::from_digit('1'), Some(SideTab::Agents));
        assert_eq!(SideTab::from_digit('4'), Some(SideTab::Stats));
        assert_eq!(SideTab::from_digit('5'), None);
        assert_eq!(SideTab::from_digit('0'), None);
        assert_eq!(SideTab::at_offset(0), Some(SideTab::Agents));
        assert_eq!(SideTab::at_offset(9), Some(SideTab::Jobs));
        assert_eq!(SideTab::at_offset(30), Some(SideTab::Stats));
        assert_eq!(SideTab::at_offset(40), None);
    }
}
