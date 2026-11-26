//! ROSSO integration helpers: mapping between ROSSO office titles and the agents that play them.

use crate::model::{AgentKind, AgentState, AutoJob, DispatchMode, Scene, WaveState};

const WAVE_MAX_AGE_MS: u64 = 7_200_000;
const WAVE_DIRECT_RECENT_MS: u64 = 120_000;

const DOMAINS: &[(&str, &str)] = &[
    ("frontend", "Frontend"),
    ("backend", "Backend"),
    ("database", "Database"),
    ("devops", "DevOps"),
    ("security", "Security"),
    ("integration", "Integration"),
    ("performance", "Performance"),
    ("qa", "QA"),
    ("research", "Research"),
];

/// ROSSO title for an agy router role (`council-architect`, `manager` + domain, …) or a relay
/// subagent type (`agy-council-architect`, …). None when the role is not a ROSSO one.
pub fn title_for(role: &str, domain: Option<&str>) -> Option<String> {
    let r = role.strip_prefix("agy-").unwrap_or(role);
    let t = match r {
        "council-architect" => "Chief Architect".to_string(),
        "council-advisor" => "Review Seat".to_string(),
        "architect" => "Design Engineer".to_string(),
        "manager" => match domain.map(|d| d.trim().to_ascii_lowercase()) {
            Some(d) if !d.is_empty() => {
                let name = DOMAINS.iter().find(|(k, _)| *k == d).map(|(_, v)| (*v).to_string()).unwrap_or_else(|| {
                    let mut c = d.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                });
                format!("{name} Head")
            }
            _ => "Department Head".to_string(),
        },
        "worker" => "Technician".to_string(),
        "tester" => "Test Inspector".to_string(),
        "supervisor" => "Chief Inspector".to_string(),
        "git" => "Records Clerk".to_string(),
        _ => return None,
    };
    Some(t)
}

/// The relay subagent type that plays a ROSSO title (inverse of `title_for`).
pub fn agent_type_for(title: &str) -> Option<&'static str> {
    let t = title.trim();
    match t {
        "Chief Architect" => Some("agy-council-architect"),
        "Design Engineer" => Some("agy-architect"),
        "Technician" => Some("agy-worker"),
        "Test Inspector" => Some("agy-tester"),
        "Chief Inspector" => Some("agy-supervisor"),
        "Records Clerk" => Some("agy-git"),
        _ if t.ends_with(" Seat") => Some("agy-council-advisor"),
        _ if t.ends_with(" Head") => Some("agy-manager"),
        _ => None,
    }
}

/// ROSSO desk directory name for an office title: "Chief Inspector" -> "chief-inspector".
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Council round step of a ROSSO document type: Task Card 0, Critique 1, Rebuttal 2, Decision Record 3.
pub fn council_step(doc_type: &str) -> Option<usize> {
    match doc_type {
        "Task Card" => Some(0),
        "Critique" => Some(1),
        "Rebuttal" => Some(2),
        "Decision Record" => Some(3),
        _ => None,
    }
}

/// The council round of the most recent ROSSO job that produced council documents.
#[derive(Debug, Clone, PartialEq)]
pub struct CouncilRound {
    pub job: String,
    /// Which of the four steps have a document (Task Card, Critique, Rebuttal, Decision Record).
    pub reached: [bool; 4],
}

impl CouncilRound {
    /// Highest step reached.
    pub fn current(&self) -> usize {
        (0..4).rev().find(|i| self.reached[*i]).unwrap_or(0)
    }
}

pub fn council_round(scene: &crate::model::Scene) -> Option<CouncilRound> {
    let mut latest: Option<(u64, &str)> = None;
    for r in &scene.reports {
        let (Some(job), Some(ty)) = (scene.report_jobs.get(&r.id), scene.report_types.get(&r.id)) else { continue };
        if council_step(ty).is_some() && latest.map_or(true, |(id, _)| r.id > id) {
            latest = Some((r.id, job.as_str()));
        }
    }
    let (_, job) = latest?;
    let mut reached = [false; 4];
    for r in &scene.reports {
        if scene.report_jobs.get(&r.id).map(String::as_str) == Some(job) {
            if let Some(step) = scene.report_types.get(&r.id).and_then(|t| council_step(t)) {
                reached[step] = true;
            }
        }
    }
    Some(CouncilRound { job: job.to_string(), reached })
}

/// Verdict colour class: 0 = good (green), 1 = needs work (amber), 2 = bad (red).
pub fn verdict_class(v: &str) -> u8 {
    match v {
        "APPROVE" | "PASS" | "SUPPORT" => 0,
        "FAIL" | "OBJECT" => 2,
        _ => 1,
    }
}

/// Freshest ROSSO wave by `updated_ms`, skipping waves older than 2 hours relative to
/// `scene.now_ms` when `scene.now_ms > 0`.
pub fn latest_wave(scene: &Scene) -> Option<&WaveState> {
    scene
        .waves
        .values()
        .filter(|w| scene.now_ms == 0 || scene.now_ms.saturating_sub(w.updated_ms) <= WAVE_MAX_AGE_MS)
        .max_by_key(|w| w.updated_ms)
}

/// Most relevant `rosso-auto` job: prefers a `"running"` job first, otherwise the newest by `started_at`.
pub fn latest_auto_job(scene: &Scene) -> Option<&AutoJob> {
    scene
        .auto_jobs
        .iter()
        .max_by_key(|j| (j.status == "running", j.started_at))
}

/// 1-based `(step, total)` progress for a `rosso-auto` flow (`"council"` or `"simple"`) and phase.
/// Returns `(0, total)` for unknown phases within a known flow, or `(0, 0)` for unknown flows.
pub fn auto_phase_progress(flow: &str, phase: &str) -> (u8, u8) {
    match flow.trim().to_ascii_lowercase().as_str() {
        "simple" => {
            let step = match phase.trim().to_ascii_lowercase().as_str() {
                "open" => 1,
                "design" => 2,
                "execute" => 3,
                "inspect" | "fix" => 4,
                "report" => 5,
                _ => 0,
            };
            (step, 5)
        }
        "council" => {
            let step = match phase.trim().to_ascii_lowercase().as_str() {
                "open" => 1,
                "proposal" => 2,
                "critique" => 3,
                "arbitrate" | "rebuttal" | "recheck" => 4,
                "heads" => 5,
                "execute" => 6,
                "inspect" | "fix" => 7,
                "report" => 8,
                _ => 0,
            };
            (step, 8)
        }
        _ => (0, 0),
    }
}

/// Effective dispatch mode (`Direct`, `Relay`, `Hybrid`, or `None`) for a top-level Claude session.
pub fn dispatch_mode(scene: &Scene, session_id: &str) -> Option<DispatchMode> {
    if matches!(scene.dispatch_modes.get(session_id), Some(DispatchMode::Hybrid)) {
        return Some(DispatchMode::Hybrid);
    }

    let is_chief_of_staff = scene
        .agents
        .values()
        .filter(|a| a.kind == AgentKind::Claude && a.parent.is_none() && a.state != AgentState::Leaving)
        .max_by_key(|a| a.last_event_ms)
        .is_some_and(|a| a.id == session_id);

    let wave = latest_wave(scene);

    let has_relay = scene.agents.values().any(|a| {
        a.kind == AgentKind::ClaudeSub
            && a.parent.as_deref() == Some(session_id)
            && !matches!(a.state, AgentState::Idle | AgentState::Done | AgentState::Leaving)
            && a.role.as_deref().is_some_and(|r| r.starts_with("agy-"))
    }) || (is_chief_of_staff && wave.is_some_and(|w| w.relay_inflight > 0))
        || matches!(scene.dispatch_modes.get(session_id), Some(DispatchMode::Relay));

    let has_direct = matches!(
        scene.dispatch_modes.get(session_id),
        Some(DispatchMode::Direct | DispatchMode::Hybrid)
    ) || scene.agents.values().any(|a| {
        a.kind == AgentKind::Agy
            && a.state != AgentState::Leaving
            && a.parent.as_deref() == Some(session_id)
    }) || scene.agents.get(session_id).is_some_and(|a| {
        a.activity.contains("rosso run")
            || a.activity.contains("rosso call")
            || a.activity.contains("--run-direct")
    }) || (is_chief_of_staff
        && wave.is_some_and(|w| {
            w.done > 0
                && w.relay_inflight == 0
                && (scene.now_ms == 0 || scene.now_ms.saturating_sub(w.updated_ms) <= WAVE_DIRECT_RECENT_MS)
        }));

    match (has_direct, has_relay) {
        (true, true) => Some(DispatchMode::Hybrid),
        (true, false) => Some(DispatchMode::Direct),
        (false, true) => Some(DispatchMode::Relay),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Agent;

    fn mk_agent(id: &str, kind: AgentKind, parent: Option<&str>, state: AgentState, last_event_ms: u64) -> Agent {
        Agent {
            id: id.into(),
            kind,
            label: id.into(),
            parent: parent.map(Into::into),
            cwd: None,
            role: None,
            model: None,
            slot: None,
            pid: None,
            state,
            activity: String::new(),
            started_ms: 0,
            last_event_ms,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            cost_usd: 0.0,
            context_pct: None,
            rss_mb: None,
            tool_calls: 0,
            tool_failures: 0,
            current_tool: None,
            last_tool: None,
            last_tool_ok: None,
            resume_id: None,
            pending_permission: None,
        }
    }

    #[test]
    fn titles_for_roles_and_types() {
        assert_eq!(title_for("council-architect", None).as_deref(), Some("Chief Architect"));
        assert_eq!(title_for("agy-council-advisor", None).as_deref(), Some("Review Seat"));
        assert_eq!(title_for("manager", Some("backend")).as_deref(), Some("Backend Head"));
        assert_eq!(title_for("agy-manager", Some("devops")).as_deref(), Some("DevOps Head"));
        assert_eq!(title_for("manager", Some("qa")).as_deref(), Some("QA Head"));
        assert_eq!(title_for("worker", None).as_deref(), Some("Technician"));
        assert_eq!(title_for("agy-git", None).as_deref(), Some("Records Clerk"));
        assert_eq!(title_for("Explore", None), None);
        assert_eq!(title_for("fallback", None), None);
    }

    #[test]
    fn inverse_mapping() {
        assert_eq!(agent_type_for("Chief Architect"), Some("agy-council-architect"));
        assert_eq!(agent_type_for("Security Seat"), Some("agy-council-advisor"));
        assert_eq!(agent_type_for("Frontend Head"), Some("agy-manager"));
        assert_eq!(agent_type_for("Test Inspector"), Some("agy-tester"));
        assert_eq!(agent_type_for("Chief of Staff"), None);
        assert_eq!(agent_type_for("CEO"), None);
    }

    #[test]
    fn latest_wave_picks_freshest_and_skips_stale() {
        let mut scene = Scene { now_ms: 10_000_000, ..Default::default() };
        assert_eq!(latest_wave(&scene), None);

        scene.waves.insert(
            "stale".into(),
            WaveState { job: "stale".into(), updated_ms: 10_000_000 - WAVE_MAX_AGE_MS - 1, ..Default::default() },
        );
        assert_eq!(latest_wave(&scene), None);

        scene.waves.insert(
            "older".into(),
            WaveState { job: "older".into(), updated_ms: 10_000_000 - 1_000, ..Default::default() },
        );
        scene.waves.insert(
            "newer".into(),
            WaveState { job: "newer".into(), updated_ms: 10_000_000 - 100, ..Default::default() },
        );
        assert_eq!(latest_wave(&scene).map(|w| w.job.as_str()), Some("newer"));
    }

    #[test]
    fn dispatch_mode_variants() {
        // None by default
        let mut scene = Scene { now_ms: 1_000_000, ..Default::default() };
        scene.agents.insert("cc:cos".into(), mk_agent("cc:cos", AgentKind::Claude, None, AgentState::Thinking, 500));
        scene.agents.insert("cc:old".into(), mk_agent("cc:old", AgentKind::Claude, None, AgentState::Idle, 100));
        assert_eq!(dispatch_mode(&scene, "cc:cos"), None);
        assert_eq!(dispatch_mode(&scene, "cc:old"), None);

        // Direct via activity
        scene.agents.get_mut("cc:old").unwrap().activity = "Bash rosso run job".into();
        assert_eq!(dispatch_mode(&scene, "cc:old"), Some(DispatchMode::Direct));

        // Direct via direct Agy child
        scene.agents.insert("agy:1".into(), mk_agent("agy:1", AgentKind::Agy, Some("cc:cos"), AgentState::Running, 600));
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Direct));

        // Hybrid when both direct Agy child and live agy-* ClaudeSub child exist
        let mut sub = mk_agent("cc-sub:cos/1", AgentKind::ClaudeSub, Some("cc:cos"), AgentState::Running, 700);
        sub.role = Some("agy-worker".into());
        scene.agents.insert("cc-sub:cos/1".into(), sub);
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Hybrid));

        // Relay only when Agy child is Leaving and ClaudeSub is live
        scene.agents.get_mut("agy:1").unwrap().state = AgentState::Leaving;
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Relay));

        // Done/Idle ClaudeSub no longer counts as live relay
        scene.agents.get_mut("cc-sub:cos/1").unwrap().state = AgentState::Done;
        assert_eq!(dispatch_mode(&scene, "cc:cos"), None);

        // Chief-of-staff wave inference: relay_inflight > 0 => Relay; done > 0 & recent => Direct
        scene.waves.insert(
            "w1".into(),
            WaveState { job: "w1".into(), updated_ms: 990_000, done: 1, relay_inflight: 2, ..Default::default() },
        );
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Relay));
        assert_eq!(dispatch_mode(&scene, "cc:old"), Some(DispatchMode::Direct)); // still has activity

        scene.waves.get_mut("w1").unwrap().relay_inflight = 0;
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Direct));

        // Explicit dispatch_modes override
        scene.dispatch_modes.insert("cc:cos".into(), DispatchMode::Relay);
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Hybrid)); // Direct from wave + Relay from map
        scene.waves.clear();
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Relay));
        scene.dispatch_modes.insert("cc:cos".into(), DispatchMode::Hybrid);
        assert_eq!(dispatch_mode(&scene, "cc:cos"), Some(DispatchMode::Hybrid));
    }

    #[test]
    fn latest_auto_job_prefers_running_then_newest() {
        let mut scene = Scene::default();
        assert_eq!(latest_auto_job(&scene), None);

        scene.auto_jobs = vec![
            AutoJob { job: "old-done".into(), status: "done".into(), started_at: 100, ..Default::default() },
            AutoJob { job: "new-done".into(), status: "done".into(), started_at: 500, ..Default::default() },
        ];
        assert_eq!(latest_auto_job(&scene).map(|j| j.job.as_str()), Some("new-done"));

        scene.auto_jobs.push(AutoJob {
            job: "active".into(),
            status: "running".into(),
            started_at: 50,
            ..Default::default()
        });
        assert_eq!(latest_auto_job(&scene).map(|j| j.job.as_str()), Some("active"));

        scene.auto_jobs.push(AutoJob {
            job: "active-newer".into(),
            status: "running".into(),
            started_at: 200,
            ..Default::default()
        });
        assert_eq!(latest_auto_job(&scene).map(|j| j.job.as_str()), Some("active-newer"));
    }

    #[test]
    fn auto_phase_progress_covers_council_simple_and_unknown() {
        let council_expected = [
            ("open", (1, 8)),
            ("proposal", (2, 8)),
            ("critique", (3, 8)),
            ("arbitrate", (4, 8)),
            ("rebuttal", (4, 8)),
            ("recheck", (4, 8)),
            ("heads", (5, 8)),
            ("execute", (6, 8)),
            ("inspect", (7, 8)),
            ("fix", (7, 8)),
            ("report", (8, 8)),
            ("unknown", (0, 8)),
        ];
        for (phase, expected) in council_expected {
            assert_eq!(auto_phase_progress("council", phase), expected, "council/{phase}");
        }

        let simple_expected = [
            ("open", (1, 5)),
            ("design", (2, 5)),
            ("execute", (3, 5)),
            ("inspect", (4, 5)),
            ("fix", (4, 5)),
            ("report", (5, 5)),
            ("proposal", (0, 5)),
            ("unknown", (0, 5)),
        ];
        for (phase, expected) in simple_expected {
            assert_eq!(auto_phase_progress("simple", phase), expected, "simple/{phase}");
        }

        assert_eq!(auto_phase_progress("unknown", "open"), (0, 0));
    }
}
