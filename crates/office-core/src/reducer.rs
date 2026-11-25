use crate::alerts::Detector;
use crate::event::{Event, EventKind, SourceKind};
use crate::model::{Agent, AgentId, AgentKind, AgentState, Alert, ReportKind, Scene};
use std::collections::BTreeMap;

/// Walk-in animation length: Arriving -> Idle.
pub const ARRIVE_MS: u64 = 2_000;
/// Walk-out animation length: Leaving -> removed.
pub const LEAVE_MS: u64 = 2_000;
/// Subagents stay Done briefly before walking out.
pub const DONE_HOLD_MS: u64 = 1_800;
/// Agents with no events for this long (and not Idle/NeedsYou) start Leaving.
pub const STALE_MS: u64 = 10 * 60 * 1_000;
/// How long retired cc:/cc-sub: ids are remembered to ignore late transcript events.
pub const RETIRED_TTL_MS: u64 = 5 * 60 * 1_000;
/// Max alerts kept in Scene.alerts (oldest dropped first; newest last).
pub const MAX_ALERTS: usize = 200;
const MAX_RETIRED: usize = 512;

/// Tool name -> character state (shared by every source).
pub fn state_for_tool(tool: &str) -> AgentState {
    match tool {
        "Edit"
        | "Write"
        | "MultiEdit"
        | "NotebookEdit"
        | "write_file"
        | "edit_file"
        | "write_to_file"
        | "replace_file_content"
        | "multi_replace_file_content" => AgentState::Typing,
        "Read"
        | "Grep"
        | "Glob"
        | "WebFetch"
        | "WebSearch"
        | "LS"
        | "ToolSearch"
        | "ListMcpResourcesTool"
        | "ReadMcpResourceTool"
        | "read_file"
        | "view_file"
        | "grep_search"
        | "find_by_name"
        | "list_dir"
        | "search_web"
        | "list_resources"
        | "read_resource" => AgentState::Reading,
        "Bash"
        | "BashOutput"
        | "Monitor"
        | "run_command"
        | "TaskCreate"
        | "TaskUpdate"
        | "TaskList"
        | "TaskGet"
        | "Skill" => AgentState::Running,
        "Agent" | "Task" | "invoke_subagent" | "swarm" | "run_workflow" => AgentState::Delegating,
        "AskUserQuestion" | "ExitPlanMode" | "ask_question" | "ask_user" | "notify_user" => AgentState::NeedsYou,
        _ => AgentState::Running,
    }
}

/// Infer kind/parent for an agent first seen through a non-AgentSeen event.
fn infer_kind(id: &str) -> (AgentKind, Option<AgentId>) {
    if let Some(rest) = id.strip_prefix("cc-sub:") {
        let parent = rest.split('/').next().map(|s| format!("cc:{s}"));
        (AgentKind::ClaudeSub, parent)
    } else if id.starts_with("cc:") {
        (AgentKind::Claude, None)
    } else if id.starts_with("agy:") {
        (AgentKind::Agy, None)
    } else {
        (AgentKind::Other, None)
    }
}

fn new_agent(id: &str, kind: AgentKind, parent: Option<AgentId>, at: u64) -> Agent {
    Agent {
        id: id.to_string(),
        kind,
        label: id.to_string(),
        parent,
        cwd: None,
        role: None,
        model: None,
        slot: None,
        pid: None,
        state: AgentState::Arriving,
        activity: String::new(),
        started_ms: at,
        last_event_ms: at,
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

/// True if `id` has any live child not in Idle or Leaving.
pub fn has_live_active_children(scene: &Scene, id: &str) -> bool {
    scene.agents.values().any(|a| {
        a.parent.as_deref() == Some(id) && !matches!(a.state, AgentState::Idle | AgentState::Leaving)
    })
}

fn canonical_cwd_uncached(cwd: &str) -> String {
    // Canonicalize the deepest existing ancestor and re-append the rest, so a deleted or not-yet
    // created directory still compares equal across symlinks (e.g. /var -> /private/var on macOS).
    let path = std::path::Path::new(cwd);
    let mut base = path;
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(base) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out.to_string_lossy().into_owned();
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                base = parent;
            }
            _ => return cwd.to_string(),
        }
    }
}

/// Folds Events into the Scene. Signatures are part of the contract; bodies are owned by workstream A.
#[derive(Default)]
pub struct Reducer {
    scene: Scene,
    detector: Detector,
    /// When each Leaving agent started leaving.
    leaving_since: BTreeMap<AgentId, u64>,
    /// Tokens/cost of agents already removed, so totals never go backwards.
    retired_tokens: u64,
    retired_cost: f64,
    /// Recently retired cc:/cc-sub: ids -> retired_at_ms (prevents delayed transcript events from resurrecting ghosts).
    retired: BTreeMap<AgentId, u64>,
    /// cwd -> canonical cwd, so `fs::canonicalize` never runs repeatedly on the hot path.
    canon_cache: std::cell::RefCell<BTreeMap<String, String>>,
    /// Standalone `ag:<sid>` ids folded into a live `agy:<pid>` (later events are redirected).
    aliases: BTreeMap<AgentId, AgentId>,
    /// Parent and label of recently removed agents (a late report still finds its recipient).
    gone_info: BTreeMap<AgentId, (Option<AgentId>, String)>,
    next_report_id: u64,
}

pub const MAX_REPORTS: usize = 300;
const MAX_REPORT_BODY: usize = 16_000;
const MAX_REPORT_BRIEF: usize = 6_000;

fn cap_text(s: String, n: usize) -> String {
    if s.chars().count() <= n {
        s
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push_str("\n… (truncated)");
        t
    }
}

const MAX_CANON_CACHE: usize = 256;
const MAX_ALIASES: usize = 256;

impl Reducer {
    fn label_of(&self, id: &str) -> String {
        self.scene
            .agents
            .get(id)
            .map(|a| a.label.clone())
            .or_else(|| self.gone_info.get(id).map(|(_, l)| l.clone()))
            .unwrap_or_else(|| id.to_string())
    }

    fn parent_of(&self, id: &str) -> Option<AgentId> {
        match self.scene.agents.get(id) {
            Some(a) => a.parent.clone(),
            None => self.gone_info.get(id).and_then(|(p, _)| p.clone()),
        }
    }

    /// Record a hand-off. A report fills in the brief and title of the task it answers.
    fn push_report(&mut self, at: u64, kind: ReportKind, from: AgentId, to: Option<AgentId>, title: String, body: String, brief: String) {
        self.push_report_as(at, kind, from, to, title, body, brief, None, false);
    }

    /// `push_report` with optional `(from_label, to_label)` overrides, and `to_boss` to address
    /// the boss explicitly (no parent fallback for `to: None`).
    #[allow(clippy::too_many_arguments)]
    fn push_report_as(
        &mut self,
        at: u64,
        kind: ReportKind,
        from: AgentId,
        to: Option<AgentId>,
        title: String,
        body: String,
        brief: String,
        labels: Option<(String, String)>,
        to_boss: bool,
    ) {
        let to = to.or_else(|| if kind == ReportKind::Result || to_boss { None } else { self.parent_of(&from) });
        if kind == ReportKind::Report {
            if let Some(task) = self.scene.reports.iter_mut().rev().find(|r| r.kind == ReportKind::Task && r.to.as_deref() == Some(from.as_str())) {
                if task.body.is_empty() && !brief.is_empty() {
                    task.body = cap_text(brief.clone(), MAX_REPORT_BRIEF);
                }
                if !title.is_empty() && task.title.starts_with('(') {
                    task.title = title.clone();
                }
            }
        }
        self.next_report_id += 1;
        let (from_label, to_label) = match labels {
            Some(l) => l,
            None => (self.label_of(&from), to.as_deref().map(|t| self.label_of(t)).unwrap_or_else(|| "you".into())),
        };
        let r = crate::model::Report {
            id: self.next_report_id,
            at_ms: at,
            kind,
            from_label,
            to_label,
            from,
            to,
            title,
            body: cap_text(body, MAX_REPORT_BODY),
            brief: cap_text(brief, MAX_REPORT_BRIEF),
        };
        self.scene.reports.push(r);
        if self.scene.reports.len() > MAX_REPORTS {
            let n = self.scene.reports.len() - MAX_REPORTS;
            self.scene.reports.drain(..n);
        }
    }

    /// The live top-level Claude session that was active most recently (the Chief of Staff).
    fn chief_of_staff(&self) -> Option<AgentId> {
        self.scene
            .agents
            .values()
            .filter(|a| a.kind == AgentKind::Claude && a.parent.is_none() && a.state != AgentState::Leaving)
            .max_by_key(|a| a.last_event_ms)
            .map(|a| a.id.clone())
    }

    /// Resolve a ROSSO title to a live agent id. `None` is the boss (CEO); an unresolved title
    /// becomes the pseudo id `rosso:<title>` (no desk).
    fn rosso_resolve(&self, title: &str) -> Option<AgentId> {
        if title == "CEO" {
            return None;
        }
        if title == "Chief of Staff" {
            return Some(self.chief_of_staff().unwrap_or_else(|| format!("rosso:{title}")));
        }
        let agy = self
            .scene
            .agents
            .values()
            .filter(|a| a.kind == AgentKind::Agy && a.label.starts_with(title))
            .filter(|a| a.state != AgentState::Leaving)
            .max_by_key(|a| a.last_event_ms);
        if let Some(a) = agy {
            return Some(a.id.clone());
        }
        if let Some(ty) = crate::rosso::agent_type_for(title) {
            let subs: Vec<&Agent> = self
                .scene
                .agents
                .values()
                .filter(|a| a.kind == AgentKind::ClaudeSub && a.role.as_deref() == Some(ty))
                .filter(|a| a.state != AgentState::Leaving)
                .collect();
            let exact = subs.iter().filter(|a| a.label == title).max_by_key(|a| a.last_event_ms);
            if let Some(a) = exact.or_else(|| subs.iter().max_by_key(|a| a.last_event_ms)) {
                return Some(a.id.clone());
            }
        }
        Some(format!("rosso:{title}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn rosso_mail(
        &mut self,
        at: u64,
        job: String,
        from_title: String,
        to_titles: Vec<String>,
        doc_type: String,
        verdict: Option<String>,
        subject: String,
        escalation: bool,
        body: String,
    ) {
        let first_id = self.next_report_id;
        let mut title = format!("{doc_type}: {subject}");
        if let Some(v) = verdict.as_deref().filter(|v| !v.is_empty()) {
            title.push_str(&format!(" [{v}]"));
        }
        if escalation {
            title = format!("\u{26a0} {title}");
        }
        let from = self.rosso_resolve(&from_title).unwrap_or_else(|| "rosso:CEO".into());
        let from_top = self.scene.agents.get(&from).is_some_and(|a| a.kind == AgentKind::Claude && a.parent.is_none());
        let is_task = matches!(doc_type.as_str(), "Memo" | "Work Order" | "Task Card");
        let mut to_ceo = false;
        let mut cos_result_sent = false;
        for t in &to_titles {
            let to = self.rosso_resolve(t);
            let boss = to.is_none();
            let kind = if boss {
                to_ceo = true;
                if from_top { ReportKind::Result } else { ReportKind::Report }
            } else if is_task {
                ReportKind::Task
            } else {
                ReportKind::Report
            };
            if boss && kind == ReportKind::Result {
                cos_result_sent = true;
            }
            self.push_report_as(
                at,
                kind,
                from.clone(),
                to,
                title.clone(),
                body.clone(),
                String::new(),
                Some((from_title.clone(), t.clone())),
                boss,
            );
        }
        if let Some(v) = verdict.as_deref().filter(|v| !v.is_empty()) {
            self.set_verdict(&from, v);
        }
        let needs_input = to_ceo && (subject.contains("NEEDS_USER_INPUT") || body.contains("NEEDS_USER_INPUT"));
        if (escalation || needs_input) && !cos_result_sent {
            if let Some(cos) = self.chief_of_staff() {
                self.push_report_as(
                    at,
                    ReportKind::Result,
                    cos,
                    None,
                    title,
                    body,
                    String::new(),
                    Some(("Chief of Staff".into(), "CEO".into())),
                    true,
                );
            }
        }
        let ids: Vec<u64> = self.scene.reports.iter().map(|r| r.id).filter(|id| *id > first_id).collect();
        for id in ids {
            if !job.is_empty() {
                self.scene.report_jobs.insert(id, job.clone());
            }
            self.scene.report_types.insert(id, doc_type.clone());
        }
        let oldest = self.scene.reports.first().map(|r| r.id).unwrap_or(0);
        self.scene.report_jobs.retain(|id, _| *id >= oldest);
        self.scene.report_types.retain(|id, _| *id >= oldest);
    }

    /// Record a Security Desk denial (bounded tallies).
    fn guard_deny(&mut self, at: u64, agent: String, tool: String, reason: String) {
        let g = &mut self.scene.guard;
        g.deny_total = g.deny_total.saturating_add(1);
        g.last_deny_ms = at;
        g.last_agent = agent;
        g.last_tool = tool;
        let key: String = reason.chars().take(80).collect();
        if !g.by_reason.contains_key(&key) && g.by_reason.len() >= crate::model::MAX_GUARD_REASONS {
            // drop the rarest reason to stay bounded
            if let Some(rare) = g.by_reason.iter().min_by_key(|(_, n)| **n).map(|(k, _)| k.clone()) {
                g.by_reason.remove(&rare);
            }
        }
        *g.by_reason.entry(key).or_insert(0) += 1;
    }

    /// Set an agent's last verdict. Returns true when it changed.
    fn set_verdict(&mut self, id: &str, verdict: &str) -> bool {
        let v = verdict.trim().to_ascii_uppercase();
        if v.is_empty() || !self.scene.agents.contains_key(id) {
            return false;
        }
        if self.scene.verdicts.get(id) == Some(&v) {
            return false;
        }
        self.scene.verdicts.insert(id.to_string(), v);
        true
    }

    /// The live agent that plays the ROSSO desk `slug` ("chief-inspector", "security-seat", ...).
    fn agent_for_slug(&self, slug: &str) -> Option<AgentId> {
        if slug == "chief-of-staff" {
            return self.chief_of_staff();
        }
        self.scene
            .agents
            .values()
            .filter(|a| a.state != AgentState::Leaving)
            .filter(|a| match a.kind {
                AgentKind::Agy => crate::rosso::slug(a.label.split('\u{b7}').next().unwrap_or("")) == slug,
                AgentKind::ClaudeSub => crate::rosso::slug(&a.label) == slug,
                _ => false,
            })
            .max_by_key(|a| a.last_event_ms)
            .map(|a| a.id.clone())
    }

    fn canon(&self, cwd: &str) -> String {
        if let Some(v) = self.canon_cache.borrow().get(cwd) {
            return v.clone();
        }
        let v = canonical_cwd_uncached(cwd);
        let mut c = self.canon_cache.borrow_mut();
        if c.len() >= MAX_CANON_CACHE {
            c.clear();
        }
        c.insert(cwd.to_string(), v.clone());
        v
    }

    /// The unique live agent whose id starts with `prefix` and whose canonical cwd is `canon`.
    fn unique_live_by_cwd(&self, prefix: &str, kind: Option<AgentKind>, canon: &str) -> Option<AgentId> {
        let mut found = self.scene.agents.values().filter(|a| {
            a.id.starts_with(prefix)
                && kind.is_none_or(|k| a.kind == k)
                && a.state != AgentState::Leaving
                && a.cwd.is_some()
                && self.same_tree_as(a, canon)
        });
        let first = found.next()?.id.clone();
        if found.next().is_some() { None } else { Some(first) }
    }

    /// The Claude subagent that relays to this agy call: a live `agy-<role>` subagent (e.g.
    /// `agy-council-architect` for router role `council-architect`) in the same project tree that
    /// has no agy call yet. With several (parallel managers), the oldest one wins.
    fn relay_sub_for(&self, agy_id: &str, role: Option<&str>, canon: &str) -> Option<AgentId> {
        let role = role?;
        let want = format!("agy-{role}");
        self.scene
            .agents
            .values()
            .filter(|a| {
                a.kind == AgentKind::ClaudeSub
                    && a.state != AgentState::Leaving
                    && a.role.as_deref() == Some(want.as_str())
                    && self.same_tree_as(a, canon)
                    && !self.scene.agents.values().any(|c| {
                        c.id != agy_id && c.kind == AgentKind::Agy && c.state != AgentState::Leaving && c.parent.as_deref() == Some(a.id.as_str())
                    })
            })
            .min_by_key(|a| (a.started_ms, a.id.clone()))
            .map(|a| a.id.clone())
    }

    /// True when `a` (or its parent session, for subagents without a cwd) works in the same
    /// directory tree as `canon` (one path contains the other).
    fn same_tree_as(&self, a: &Agent, canon: &str) -> bool {
        let cwd = a.cwd.clone().or_else(|| a.parent.as_ref().and_then(|p| self.scene.agents.get(p)).and_then(|p| p.cwd.clone()));
        let Some(c) = cwd else { return true };
        let c = self.canon(&c);
        let within = |x: &str, y: &str| x == y || x.starts_with(&format!("{}/", y.trim_end_matches('/')));
        within(&c, canon) || within(canon, &c)
    }

    /// Redirect events for a folded `ag:<sid>` to its `agy:<pid>`; fold a late-arriving `ag:<sid>`
    /// into a unique live `agy:<pid>` with the same cwd.
    fn remap(&mut self, kind: &mut EventKind) {
        let late_ag = match &*kind {
            EventKind::AgentSeen { id, cwd: Some(cwd), .. } if id.starts_with("ag:") && !self.aliases.contains_key(id.as_str()) => {
                Some((id.clone(), self.canon(cwd)))
            }
            _ => None,
        };
        if let Some((ag_id, canon)) = late_ag {
            if let Some(target) = self.unique_live_by_cwd("agy:", None, &canon) {
                if self.aliases.len() >= MAX_ALIASES {
                    self.aliases.pop_first();
                }
                self.aliases.insert(ag_id, target);
            }
        }
        let id_ref: Option<&mut AgentId> = match kind {
            EventKind::AgentSeen { id, .. }
            | EventKind::State { id, .. }
            | EventKind::ToolStart { id, .. }
            | EventKind::ToolEnd { id, .. }
            | EventKind::Usage { id, .. }
            | EventKind::Resources { id, .. }
            | EventKind::PermissionRequested { id, .. }
            | EventKind::PermissionResolved { id, .. }
            | EventKind::Verdict { id, .. }
            | EventKind::Failure { id, .. }
            | EventKind::Gone { id } => Some(id),
            _ => None,
        };
        let Some(id) = id_ref else { return };
        let Some(target) = self.aliases.get(id.as_str()).cloned() else { return };
        let Some(t) = self.scene.agents.get(&target) else {
            self.aliases.remove(id.as_str());
            return;
        };
        let target_kind = t.kind;
        *id = target;
        if let EventKind::AgentSeen { kind: k, label, parent, .. } = kind {
            *k = target_kind;
            *label = None;
            *parent = None;
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Reducer whose alert thresholds come from `cfg` (cost_alert_usd, stuck_after_s).
    pub fn with_config(cfg: &crate::config::Config) -> Self {
        Reducer { detector: Detector::new(cfg), ..Self::default() }
    }

    fn push_alerts(&mut self, alerts: Vec<Alert>) {
        if alerts.is_empty() {
            return;
        }
        self.scene.alerts.extend(alerts);
        let n = self.scene.alerts.len();
        if n > MAX_ALERTS {
            self.scene.alerts.drain(..n - MAX_ALERTS);
        }
    }

    /// Clear all alerts (TUI `c`).
    pub fn clear_alerts(&mut self) {
        self.scene.alerts.clear();
    }

    fn recompute_totals(&mut self) {
        let (mut t, mut c) = (self.retired_tokens, self.retired_cost);
        for a in self.scene.agents.values() {
            t += a.tokens_shown();
            c += a.cost_usd;
        }
        self.scene.total_tokens = t;
        self.scene.total_cost_usd = c;
    }

    /// Get or create (state Arriving) an agent for events that reference an unknown id.
    fn ensure(&mut self, id: &str, at: u64) -> &mut Agent {
        if !self.scene.agents.contains_key(id) {
            let (kind, parent) = infer_kind(id);
            self.scene.agents.insert(id.to_string(), new_agent(id, kind, parent, at));
        }
        let a = self.scene.agents.get_mut(id).expect("just inserted");
        a.last_event_ms = a.last_event_ms.max(at);
        a
    }

    fn set_state(&mut self, id: &str, state: AgentState, at: u64) {
        if let Some(a) = self.scene.agents.get_mut(id) {
            a.state = state;
            if state == AgentState::Leaving {
                self.leaving_since.entry(id.to_string()).or_insert(at);
            } else {
                self.leaving_since.remove(id);
            }
        }
    }

    fn is_retired(&self, id: &str, now_ms: u64) -> bool {
        self.retired
            .get(id)
            .is_some_and(|&retired_at| now_ms.saturating_sub(retired_at) < RETIRED_TTL_MS)
    }

    fn retire_agent(&mut self, id: String, now_ms: u64) {
        if !(id.starts_with("cc:") || id.starts_with("cc-sub:")) {
            return;
        }
        self.retired.insert(id, now_ms);
        if self.retired.len() > MAX_RETIRED {
            self.retired.retain(|_, &mut t| now_ms.saturating_sub(t) < RETIRED_TTL_MS);
            while self.retired.len() > MAX_RETIRED {
                if let Some(oldest_id) = self
                    .retired
                    .iter()
                    .min_by_key(|(_, t)| *t)
                    .map(|(k, _)| k.clone())
                {
                    self.retired.remove(&oldest_id);
                } else {
                    break;
                }
            }
        }
    }

    fn refresh_parent(&mut self, child_id: &str, at: u64) {
        let Some(parent_id) = self.scene.agents.get(child_id).and_then(|a| a.parent.clone()) else {
            return;
        };
        if let Some(p) = self.scene.agents.get_mut(&parent_id) {
            if p.state != AgentState::Leaving {
                p.last_event_ms = p.last_event_ms.max(at);
            }
        }
    }

    fn disambiguate_sub_label(&self, id: &str, parent: Option<&str>, base: &str) -> String {
        let taken: std::collections::HashSet<&str> = self
            .scene
            .agents
            .values()
            .filter(|a| {
                a.id != id
                    && a.kind == AgentKind::ClaudeSub
                    && a.parent.as_deref() == parent
                    && a.state != AgentState::Leaving
            })
            .map(|a| a.label.as_str())
            .collect();
        if !taken.contains(base) {
            return base.to_string();
        }
        for n in 2..=999 {
            let candidate = format!("{base}#{n}");
            if !taken.contains(candidate.as_str()) {
                return candidate;
            }
        }
        format!("{base}#?")
    }

    fn cascade_leaving(&mut self, parent_id: &str, at: u64) -> bool {
        let kids: Vec<AgentId> = self
            .scene
            .agents
            .values()
            .filter(|a| a.parent.as_deref() == Some(parent_id) && a.state != AgentState::Leaving)
            .map(|a| a.id.clone())
            .collect();
        let any = !kids.is_empty();
        for kid in kids {
            if let Some(a) = self.scene.agents.get_mut(&kid) {
                a.pending_permission = None;
            }
            self.set_state(&kid, AgentState::Leaving, at);
        }
        any
    }

    /// Apply one event. Returns true if the scene changed.
    pub fn apply(&mut self, mut ev: Event) -> bool {
        self.remap(&mut ev.kind);
        let at = ev.at_ms;
        self.scene.now_ms = self.scene.now_ms.max(at);
        let alerts = self.detector.observe(&ev, &self.scene);
        let had_alerts = !alerts.is_empty();
        self.push_alerts(alerts);

        let mut refresh_target: Option<AgentId> = None;
        let changed = match ev.kind {
            EventKind::AgentSeen { id, kind, label, parent, cwd, role, model, slot, pid, resume_id } => {
                if ev.source == SourceKind::ClaudeTranscript && self.is_retired(&id, at) {
                    return had_alerts;
                }
                if ev.source != SourceKind::ClaudeTranscript {
                    self.retired.remove(&id);
                }

                // agy:<pid> reconciliation with standalone ag:<sid> and parent inference
                let mut merged_ag: Option<Agent> = None;
                let mut inferred_parent = parent;
                if let (true, Some(raw_cwd)) = (id.starts_with("agy:"), cwd.as_ref()) {
                    let canon = self.canon(raw_cwd);
                    if let Some(ag_id) = self.unique_live_by_cwd("ag:", None, &canon) {
                        self.leaving_since.remove(&ag_id);
                        merged_ag = self.scene.agents.remove(&ag_id);
                        if self.aliases.len() >= MAX_ALIASES {
                            self.aliases.pop_first();
                        }
                        self.aliases.insert(ag_id, id.clone());
                    }
                    if inferred_parent.is_none() {
                        inferred_parent = self
                            .relay_sub_for(&id, role.as_deref(), &canon)
                            .or_else(|| self.unique_live_by_cwd("", Some(AgentKind::Claude), &canon));
                    }
                }

                let exists = self.scene.agents.contains_key(&id);
                let had_parent = self.scene.agents.get(&id).is_some_and(|a| a.parent.is_some());
                if !exists {
                    self.scene.agents.insert(id.clone(), new_agent(&id, kind, None, at));
                }
                let revive = self.scene.agents.get(&id).map(|a| a.state == AgentState::Leaving).unwrap_or(false);

                let effective_parent = inferred_parent
                    .or_else(|| merged_ag.as_ref().and_then(|m| m.parent.clone()))
                    .or_else(|| self.scene.agents.get(&id).and_then(|a| a.parent.clone()));
                let disambiguated_label = if kind == AgentKind::ClaudeSub {
                    label.map(|l| self.disambiguate_sub_label(&id, effective_parent.as_deref(), &l))
                } else {
                    label
                };

                let a = self.scene.agents.get_mut(&id).expect("present");
                a.kind = kind;
                a.last_event_ms = a.last_event_ms.max(at);
                if let Some(m) = merged_ag {
                    a.started_ms = a.started_ms.min(m.started_ms);
                    a.tokens_in += m.tokens_in;
                    a.tokens_out += m.tokens_out;
                    a.tokens_cache_read += m.tokens_cache_read;
                    a.tokens_cache_write += m.tokens_cache_write;
                    a.cost_usd += m.cost_usd;
                    if a.context_pct.is_none() {
                        a.context_pct = m.context_pct;
                    }
                    a.tool_calls += m.tool_calls;
                    a.tool_failures += m.tool_failures;
                    if a.activity.is_empty() {
                        a.activity = m.activity;
                    }
                    if a.current_tool.is_none() {
                        a.current_tool = m.current_tool;
                    }
                    if a.last_tool.is_none() {
                        a.last_tool = m.last_tool;
                        a.last_tool_ok = m.last_tool_ok;
                    }
                    if a.state == AgentState::Arriving {
                        a.state = m.state;
                    }
                }
                if let Some(v) = disambiguated_label {
                    a.label = v;
                }
                if effective_parent.is_some() {
                    a.parent = effective_parent;
                }
                if cwd.is_some() {
                    a.cwd = cwd;
                }
                if role.is_some() {
                    a.role = role;
                }
                if model.is_some() {
                    a.model = model;
                }
                if slot.is_some() {
                    a.slot = slot;
                }
                if pid.is_some() {
                    a.pid = pid;
                }
                if resume_id.is_some() {
                    a.resume_id = resume_id;
                }
                if revive {
                    self.set_state(&id, AgentState::Idle, at);
                }
                // A child just got its boss: that's a brief being handed down.
                let new_link = self.scene.agents.get(&id).and_then(|a| (!had_parent).then(|| a.parent.clone()).flatten().map(|p| (p, a.kind, a.role.clone(), a.label.clone())));
                if let Some((p, k, role, label)) = new_link {
                    if k != AgentKind::Claude {
                        let title = format!("({})", role.unwrap_or(label));
                        self.push_report(at, ReportKind::Task, p, Some(id.clone()), title, String::new(), String::new());
                    }
                }
                refresh_target = Some(id);
                true
            }
            EventKind::State { id, state, activity } => {
                let a = self.ensure(&id, at);
                if let Some(act) = activity {
                    a.activity = act;
                }
                if matches!(state, AgentState::Idle | AgentState::Done | AgentState::Leaving) {
                    a.pending_permission = None;
                }
                self.set_state(&id, state, at);
                refresh_target = Some(id);
                true
            }
            EventKind::ToolStart { id, tool, summary } => {
                let a = self.ensure(&id, at);
                a.pending_permission = None;
                a.tool_calls += 1;
                a.current_tool = Some(tool.clone());
                a.activity = if summary.is_empty() { tool.clone() } else { format!("{tool} {summary}") };
                self.set_state(&id, state_for_tool(&tool), at);
                refresh_target = Some(id);
                true
            }
            EventKind::ToolEnd { id, tool, ok } => {
                let a = self.ensure(&id, at);
                if !ok {
                    a.tool_failures += 1;
                }
                a.current_tool = None;
                a.last_tool = Some(tool);
                a.last_tool_ok = Some(ok);
                a.pending_permission = None;
                self.set_state(&id, AgentState::Thinking, at);
                refresh_target = Some(id);
                true
            }
            EventKind::Usage { id, tokens_in, tokens_out, tokens_cache_read, tokens_cache_write, cost_usd, context_pct, model } => {
                // Usage carries running totals (not deltas).
                if let Some(a) = self.scene.agents.get_mut(&id) {
                    a.tokens_in = tokens_in;
                    a.tokens_out = tokens_out;
                    a.tokens_cache_read = tokens_cache_read;
                    a.tokens_cache_write = tokens_cache_write;
                    a.cost_usd = cost_usd;
                    if context_pct.is_some() {
                        a.context_pct = context_pct;
                    }
                    if model.is_some() {
                        a.model = model;
                    }
                    if a.state != AgentState::Leaving {
                        a.last_event_ms = a.last_event_ms.max(at);
                    }
                    refresh_target = Some(id);
                    true
                } else if self.is_retired(&id, at) || id.starts_with("cc-sub:") {
                    false
                } else {
                    let a = self.ensure(&id, at);
                    a.tokens_in = tokens_in;
                    a.tokens_out = tokens_out;
                    a.tokens_cache_read = tokens_cache_read;
                    a.tokens_cache_write = tokens_cache_write;
                    a.cost_usd = cost_usd;
                    if context_pct.is_some() {
                        a.context_pct = context_pct;
                    }
                    if model.is_some() {
                        a.model = model;
                    }
                    refresh_target = Some(id);
                    true
                }
            }
            EventKind::Resources { id, rss_mb } => match self.scene.agents.get_mut(&id) {
                Some(a) => {
                    a.last_event_ms = a.last_event_ms.max(at);
                    let ch = a.rss_mb != Some(rss_mb);
                    a.rss_mb = Some(rss_mb);
                    refresh_target = Some(id);
                    ch
                }
                None => false,
            },
            EventKind::PermissionRequested { id, request_id, summary } => {
                let a = self.ensure(&id, at);
                a.pending_permission = Some(request_id);
                a.activity = summary;
                self.set_state(&id, AgentState::NeedsYou, at);
                refresh_target = Some(id);
                true
            }
            EventKind::PermissionResolved { id, request_id } => match self.scene.agents.get_mut(&id) {
                Some(a) if a.pending_permission.as_deref() == Some(request_id.as_str()) => {
                    a.pending_permission = None;
                    a.last_event_ms = a.last_event_ms.max(at);
                    if a.state == AgentState::NeedsYou {
                        self.set_state(&id, AgentState::Thinking, at);
                    }
                    refresh_target = Some(id);
                    true
                }
                _ => false,
            },
            EventKind::Gone { id } => {
                if let Some(a) = self.scene.agents.get_mut(&id) {
                    a.pending_permission = None;
                    a.last_event_ms = a.last_event_ms.max(at);
                    self.set_state(&id, AgentState::Leaving, at);
                    self.cascade_leaving(&id, at);
                    refresh_target = Some(id);
                    true
                } else {
                    self.cascade_leaving(&id, at)
                }
            }
            EventKind::Router(status) => {
                self.scene.router = status;
                true
            }
            EventKind::Alert(_) => false, // already passed through by the Detector
            EventKind::RouterExtra(x) => {
                let changed = self.scene.router_extra != x;
                self.scene.router_extra = x;
                changed
            }
            EventKind::GuardDecision { agent, tool, reason, deny } => {
                if deny {
                    self.guard_deny(at, agent, tool, reason);
                }
                deny
            }
            EventKind::Verdict { id, verdict } => self.set_verdict(&id, &verdict),
            EventKind::Failure { id, info } => {
                if self.scene.agents.contains_key(&id) {
                    self.scene.failures.insert(id, info);
                    true
                } else {
                    false
                }
            }
            EventKind::Entrypoint { id, entrypoint } => {
                if self.scene.agents.contains_key(&id) && self.scene.entrypoints.get(&id) != Some(&entrypoint) {
                    self.scene.entrypoints.insert(id, entrypoint);
                    true
                } else {
                    false
                }
            }
            EventKind::InTray { slug, count } => match self.agent_for_slug(&slug) {
                Some(id) => {
                    if count == 0 {
                        self.scene.in_trays.remove(&id).is_some()
                    } else {
                        self.scene.in_trays.insert(id, count) != Some(count)
                    }
                }
                None => false,
            },
            EventKind::Handoff { from, to, kind, title, body, brief } => {
                self.push_report(at, kind, from, to, title, body, brief);
                true
            }
            EventKind::RossoMail { job, from_title, to_titles, doc_type, verdict, subject, escalation, body, .. } => {
                self.rosso_mail(at, job, from_title, to_titles, doc_type, verdict, subject, escalation, body);
                true
            }
        };
        if let Some(ref child_id) = refresh_target {
            self.refresh_parent(child_id, at);
        }
        if changed {
            self.recompute_totals();
        }
        changed || had_alerts
    }

    /// Periodic housekeeping: Arriving->Idle after walk-in, stale/Gone agents -> Leaving -> removed,
    /// stuck detection hand-off to alerts. Called ~every 500 ms.
    pub fn sweep(&mut self, now_ms: u64) -> bool {
        self.scene.now_ms = self.scene.now_ms.max(now_ms);
        let mut changed = false;
        let mut to_idle = Vec::new();
        let mut to_leave = Vec::new();
        let mut to_remove = Vec::new();
        for (id, a) in &self.scene.agents {
            match a.state {
                AgentState::Arriving if now_ms.saturating_sub(a.started_ms) >= ARRIVE_MS => to_idle.push(id.clone()),
                AgentState::Leaving => {
                    let since = self.leaving_since.get(id).copied().unwrap_or(now_ms);
                    if now_ms.saturating_sub(since) >= LEAVE_MS {
                        to_remove.push(id.clone());
                    } else if !self.leaving_since.contains_key(id) {
                        to_leave.push(id.clone()); // Leaving without a timestamp: start the clock now
                    }
                }
                AgentState::Idle | AgentState::NeedsYou => {}
                AgentState::Done
                    if a.kind == AgentKind::ClaudeSub
                        && now_ms.saturating_sub(a.last_event_ms) >= DONE_HOLD_MS =>
                {
                    to_leave.push(id.clone());
                }
                _ if now_ms.saturating_sub(a.last_event_ms) >= STALE_MS
                    && !has_live_active_children(&self.scene, id) =>
                {
                    to_leave.push(id.clone());
                }
                _ => {}
            }
        }
        for id in to_idle {
            self.set_state(&id, AgentState::Idle, now_ms);
            changed = true;
        }
        for id in to_leave {
            if let Some(a) = self.scene.agents.get_mut(&id) {
                a.state = AgentState::Leaving;
                a.last_event_ms = a.last_event_ms.max(now_ms);
            }
            self.leaving_since.insert(id, now_ms);
            changed = true;
        }
        for id in to_remove {
            if let Some(a) = self.scene.agents.get(&id) {
                if self.gone_info.len() >= 512 {
                    self.gone_info.pop_first();
                }
                self.gone_info.insert(id.clone(), (a.parent.clone(), a.label.clone()));
            }
            if let Some(a) = self.scene.agents.remove(&id) {
                self.retired_tokens += a.tokens_shown();
                self.retired_cost += a.cost_usd;
            }
            self.cascade_leaving(&id, now_ms);
            self.scene.verdicts.remove(&id);
            self.scene.failures.remove(&id);
            self.scene.entrypoints.remove(&id);
            self.scene.in_trays.remove(&id);
            self.retire_agent(id.clone(), now_ms);
            self.leaving_since.remove(&id);
            changed = true;
        }
        let alerts = self.detector.tick(&self.scene, now_ms);
        if !alerts.is_empty() {
            self.push_alerts(alerts);
            changed = true;
        }
        if changed {
            self.recompute_totals();
        }
        changed
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SourceKind;
    use crate::model::{AlertLevel, RouterStatus};

    fn ev(at: u64, kind: EventKind) -> Event {
        Event { at_ms: at, source: SourceKind::Demo, kind }
    }
    fn seen(id: &str, label: Option<&str>, cwd: Option<&str>) -> EventKind {
        EventKind::AgentSeen {
            id: id.into(),
            kind: AgentKind::Claude,
            label: label.map(Into::into),
            parent: None,
            cwd: cwd.map(Into::into),
            role: None,
            model: Some("opus".into()),
            slot: None,
            pid: None,
            resume_id: None,
        }
    }
    fn codes(r: &Reducer) -> Vec<String> {
        r.scene().alerts.iter().map(|a| a.code.clone()).collect()
    }

    #[test]
    fn agent_seen_upsert_keeps_existing_fields() {
        let mut r = Reducer::new();
        assert!(r.apply(ev(1, seen("cc:a", Some("cc·DICE"), Some("/x/DICE")))));
        r.apply(ev(2, seen("cc:a", None, None)));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.label, "cc·DICE");
        assert_eq!(a.cwd.as_deref(), Some("/x/DICE"));
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!(a.state, AgentState::Arriving);
        assert_eq!(a.started_ms, 1);
        assert_eq!(a.last_event_ms, 2);
    }

    #[test]
    fn arriving_to_idle_after_2s() {
        let mut r = Reducer::new();
        r.apply(ev(1000, seen("cc:a", None, None)));
        assert!(!r.sweep(2500));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Arriving);
        assert!(r.sweep(3000));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Idle);
    }

    #[test]
    fn tool_states_and_counters() {
        let mut r = Reducer::new();
        r.apply(ev(1, seen("cc:a", None, None)));
        r.apply(ev(2, EventKind::ToolStart { id: "cc:a".into(), tool: "Edit".into(), summary: "main.rs".into() }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::Typing);
        assert_eq!(a.activity, "Edit main.rs");
        r.apply(ev(3, EventKind::ToolEnd { id: "cc:a".into(), tool: "Edit".into(), ok: false }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::Thinking);
        assert_eq!((a.tool_calls, a.tool_failures), (1, 1));
        for (t, s) in [("Grep", AgentState::Reading), ("Bash", AgentState::Running), ("Task", AgentState::Delegating)] {
            r.apply(ev(4, EventKind::ToolStart { id: "cc:a".into(), tool: t.into(), summary: String::new() }));
            assert_eq!(r.scene().agents["cc:a"].state, s);
        }
    }

    #[test]
    fn unknown_sub_is_created_with_parent() {
        let mut r = Reducer::new();
        r.apply(ev(1, EventKind::State { id: "cc-sub:s1/ag1".into(), state: AgentState::Thinking, activity: None }));
        let a = &r.scene().agents["cc-sub:s1/ag1"];
        assert_eq!(a.kind, AgentKind::ClaudeSub);
        assert_eq!(a.parent.as_deref(), Some("cc:s1"));
    }

    #[test]
    fn usage_totals_survive_removal() {
        let mut r = Reducer::new();
        r.apply(ev(1, seen("cc:a", None, None)));
        r.apply(ev(1, seen("cc:b", None, None)));
        let u = |id: &str, i, o, c| EventKind::Usage {
            id: id.into(),
            tokens_in: i,
            tokens_out: o,
            tokens_cache_read: 1_000_000,
            tokens_cache_write: 0,
            cost_usd: c,
            context_pct: Some(10.0),
            model: None,
        };
        r.apply(ev(2, u("cc:a", 100, 50, 1.0)));
        r.apply(ev(3, u("cc:a", 200, 60, 2.0))); // running totals, not deltas
        r.apply(ev(3, u("cc:b", 10, 0, 0.5)));
        assert_eq!(r.scene().total_tokens, 270);
        assert!((r.scene().total_cost_usd - 2.5).abs() < 1e-9);
        r.apply(ev(4, EventKind::Gone { id: "cc:a".into() }));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Leaving);
        r.sweep(5000);
        assert!(r.sweep(6100) || !r.scene().agents.contains_key("cc:a"));
        assert!(!r.scene().agents.contains_key("cc:a"));
        assert_eq!(r.scene().total_tokens, 270);
        assert!((r.scene().total_cost_usd - 2.5).abs() < 1e-9);
    }

    #[test]
    fn gone_then_removed_after_2s() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(10_000, EventKind::Gone { id: "cc:a".into() }));
        r.sweep(11_000);
        assert!(r.scene().agents.contains_key("cc:a"));
        r.sweep(12_000);
        assert!(!r.scene().agents.contains_key("cc:a"));
    }

    #[test]
    fn stale_agents_leave_but_idle_and_needs_you_stay() {
        let mut r = Reducer::new();
        for id in ["cc:busy", "cc:idle", "cc:ask"] {
            r.apply(ev(0, seen(id, None, None)));
        }
        r.apply(ev(1, EventKind::State { id: "cc:busy".into(), state: AgentState::Running, activity: None }));
        r.apply(ev(1, EventKind::State { id: "cc:idle".into(), state: AgentState::Idle, activity: None }));
        r.apply(ev(1, EventKind::State { id: "cc:ask".into(), state: AgentState::NeedsYou, activity: None }));
        let t = STALE_MS + 10;
        r.sweep(t);
        assert_eq!(r.scene().agents["cc:busy"].state, AgentState::Leaving);
        assert_eq!(r.scene().agents["cc:idle"].state, AgentState::Idle);
        assert_eq!(r.scene().agents["cc:ask"].state, AgentState::NeedsYou);
        r.sweep(t + LEAVE_MS);
        assert!(!r.scene().agents.contains_key("cc:busy"));
    }

    #[test]
    fn permission_flow() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::PermissionRequested { id: "cc:a".into(), request_id: "r1".into(), summary: "Bash rm".into() }));
        let a = &r.scene().agents["cc:a"];
        assert_eq!(a.state, AgentState::NeedsYou);
        assert_eq!(a.pending_permission.as_deref(), Some("r1"));
        assert!(codes(&r).contains(&"needs_you".to_string()));
        r.apply(ev(2, EventKind::PermissionResolved { id: "cc:a".into(), request_id: "other".into() }));
        assert!(r.scene().agents["cc:a"].pending_permission.is_some());
        r.apply(ev(3, EventKind::PermissionResolved { id: "cc:a".into(), request_id: "r1".into() }));
        let a = &r.scene().agents["cc:a"];
        assert!(a.pending_permission.is_none());
        assert_eq!(a.state, AgentState::Thinking);
    }

    #[test]
    fn loop_alert_after_four_repeats() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        for i in 0..5 {
            r.apply(ev(i, EventKind::ToolStart { id: "cc:a".into(), tool: "Bash".into(), summary: "npm test".into() }));
        }
        assert_eq!(codes(&r).iter().filter(|c| *c == "loop").count(), 1);
    }

    #[test]
    fn failure_streak_alert() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        let end = |ok| EventKind::ToolEnd { id: "cc:a".into(), tool: "Bash".into(), ok };
        r.apply(ev(1, end(false)));
        r.apply(ev(2, end(false)));
        r.apply(ev(3, end(true)));
        r.apply(ev(4, end(false)));
        r.apply(ev(5, end(false)));
        assert!(codes(&r).is_empty());
        r.apply(ev(6, end(false)));
        assert_eq!(codes(&r), vec!["tool_failures"]);
    }

    #[test]
    fn stuck_alert_once_per_quiet_period() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1000, EventKind::State { id: "cc:a".into(), state: AgentState::Running, activity: None }));
        r.sweep(100_000);
        assert!(codes(&r).is_empty());
        r.sweep(122_000);
        r.sweep(125_000);
        assert_eq!(codes(&r), vec!["stuck"]);
        // new activity re-arms it
        r.apply(ev(130_000, EventKind::ToolStart { id: "cc:a".into(), tool: "Bash".into(), summary: "x".into() }));
        r.sweep(260_000);
        assert_eq!(codes(&r), vec!["stuck", "stuck"]);
    }

    #[test]
    fn cost_alert_fires_at_each_multiple() {
        let mut r = Reducer::new(); // default cost_alert_usd = 50
        r.apply(ev(0, seen("cc:a", None, None)));
        for c in [1.0, 5.5, 49.9, 50.5, 70.0, 99.0, 100.0, 160.0, 120.0, 161.0] {
            r.apply(ev(1, EventKind::Usage {
                id: "cc:a".into(),
                tokens_in: 1,
                tokens_out: 1,
                tokens_cache_read: 0,
                tokens_cache_write: 0,
                cost_usd: c,
                context_pct: None,
                model: None,
            }));
        }
        // $50, $100, $150 (the jump to 160 crosses one more multiple); no repeats
        assert_eq!(codes(&r), vec!["cost", "cost", "cost"]);
        assert_eq!(r.scene().alerts[0].level, AlertLevel::Critical);
        assert!(r.scene().alerts[2].message.contains("$150.00"), "{}", r.scene().alerts[2].message);
    }

    #[test]
    fn usage_without_cache_fields_still_parses() {
        // events/agents serialised before the cache split default the cache fields to 0
        let ev: EventKind = serde_json::from_str(
            r#"{"Usage":{"id":"cc:a","tokens_in":5,"tokens_out":2,"cost_usd":0.1,"context_pct":null}}"#,
        )
        .unwrap();
        assert!(matches!(ev, EventKind::Usage { tokens_cache_read: 0, tokens_cache_write: 0, .. }));
        let mut a = serde_json::to_value(new_agent("cc:a", AgentKind::Claude, None, 0)).unwrap();
        a.as_object_mut().unwrap().remove("tokens_cache_read");
        a.as_object_mut().unwrap().remove("tokens_cache_write");
        let a: crate::model::Agent = serde_json::from_value(a).unwrap();
        assert_eq!((a.tokens_cache_read, a.tokens_cache_write), (0, 0));
    }

    #[test]
    fn router_alerts_pass_through_and_cap() {
        let mut r = Reducer::new();
        r.apply(ev(0, EventKind::Router(RouterStatus { capacity: Some(3), ..Default::default() })));
        assert_eq!(r.scene().router.capacity, Some(3));
        for i in 0..(MAX_ALERTS as u64 + 50) {
            let a = Alert { at_ms: i, level: AlertLevel::Warn, agent: None, code: "timeout".into(), message: format!("m{i}") };
            assert!(r.apply(ev(i, EventKind::Alert(a))));
        }
        let al = &r.scene().alerts;
        assert_eq!(al.len(), MAX_ALERTS);
        assert_eq!(al.last().unwrap().message, format!("m{}", MAX_ALERTS + 49));
        assert_eq!(al[0].message, "m50");
    }

    #[test]
    fn needs_you_state_alert_on_transition_only() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        let s = || EventKind::State { id: "cc:a".into(), state: AgentState::NeedsYou, activity: Some("permission".into()) };
        r.apply(ev(1, s()));
        r.apply(ev(2, s()));
        assert_eq!(codes(&r), vec!["needs_you"]);
    }

    #[test]
    fn agent_seen_revives_leaving_agent() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::Gone { id: "cc:a".into() }));
        r.apply(ev(2, seen("cc:a", None, None)));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Idle);
        r.sweep(10_000);
        assert!(r.scene().agents.contains_key("cc:a"));
    }

    #[test]
    fn parent_gone_cascades_and_post_gone_usage_does_not_resurrect_sub() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:p", None, None)));
        r.apply(ev(1, EventKind::AgentSeen {
            id: "cc-sub:p/1".into(),
            kind: AgentKind::ClaudeSub,
            label: Some("Explore".into()),
            parent: Some("cc:p".into()),
            cwd: None,
            role: None,
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }));
        r.apply(ev(2, EventKind::State { id: "cc-sub:p/1".into(), state: AgentState::Reading, activity: None }));
        r.apply(ev(3, EventKind::Gone { id: "cc:p".into() }));
        assert_eq!(r.scene().agents["cc:p"].state, AgentState::Leaving);
        assert_eq!(r.scene().agents["cc-sub:p/1"].state, AgentState::Leaving);
        r.sweep(3 + LEAVE_MS);
        assert!(!r.scene().agents.contains_key("cc:p"));
        assert!(!r.scene().agents.contains_key("cc-sub:p/1"));

        // Late Usage for the finished subagent must not resurrect it.
        assert!(!r.apply(ev(5_000, EventKind::Usage {
            id: "cc-sub:p/1".into(),
            tokens_in: 100,
            tokens_out: 50,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            cost_usd: 0.1,
            context_pct: None,
            model: None,
        })));
        assert!(!r.scene().agents.contains_key("cc-sub:p/1"));
    }

    #[test]
    fn tool_start_clears_permission_and_maps_antigravity_tools() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::PermissionRequested { id: "cc:a".into(), request_id: "req1".into(), summary: "Bash".into() }));
        assert_eq!(r.scene().agents["cc:a"].pending_permission.as_deref(), Some("req1"));
        r.apply(ev(2, EventKind::ToolStart { id: "cc:a".into(), tool: "write_to_file".into(), summary: "x.rs".into() }));
        assert!(r.scene().agents["cc:a"].pending_permission.is_none());
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Typing);
        assert_eq!(state_for_tool("read_file"), AgentState::Reading);
        assert_eq!(state_for_tool("run_command"), AgentState::Running);
        assert_eq!(state_for_tool("invoke_subagent"), AgentState::Delegating);
        assert_eq!(state_for_tool("ExitPlanMode"), AgentState::NeedsYou);
    }

    #[test]
    fn retired_ttl_blocks_transcript_but_hook_revives() {
        let mut r = Reducer::new();
        r.apply(Event { at_ms: 0, source: SourceKind::ClaudeHook, kind: seen("cc:gone", Some("gone"), None) });
        r.apply(Event { at_ms: 100, source: SourceKind::ClaudeHook, kind: EventKind::Gone { id: "cc:gone".into() } });
        r.sweep(100 + LEAVE_MS);
        assert!(!r.scene().agents.contains_key("cc:gone"));

        // Transcript AgentSeen and Usage within TTL are ignored.
        assert!(!r.apply(Event {
            at_ms: 5_000,
            source: SourceKind::ClaudeTranscript,
            kind: seen("cc:gone", Some("gone"), None),
        }));
        assert!(!r.apply(Event {
            at_ms: 5_000,
            source: SourceKind::ClaudeTranscript,
            kind: EventKind::Usage {
                id: "cc:gone".into(),
                tokens_in: 10,
                tokens_out: 5,
                tokens_cache_read: 0,
                tokens_cache_write: 0,
                cost_usd: 0.01,
                context_pct: None,
                model: None,
            },
        }));
        assert!(!r.scene().agents.contains_key("cc:gone"));

        // ClaudeHook AgentSeen removes from retired and revives.
        assert!(r.apply(Event {
            at_ms: 6_000,
            source: SourceKind::ClaudeHook,
            kind: seen("cc:gone", Some("gone"), None),
        }));
        assert!(r.scene().agents.contains_key("cc:gone"));
    }

    #[test]
    fn retired_ttl_expiry_lets_transcript_recreate() {
        let mut r = Reducer::new();
        r.apply(Event { at_ms: 0, source: SourceKind::ClaudeHook, kind: seen("cc:old", Some("old"), None) });
        r.apply(Event { at_ms: 100, source: SourceKind::ClaudeHook, kind: EventKind::Gone { id: "cc:old".into() } });
        r.sweep(100 + LEAVE_MS);
        assert!(!r.scene().agents.contains_key("cc:old"));
        let t = |at| Event { at_ms: at, source: SourceKind::ClaudeTranscript, kind: seen("cc:old", Some("old"), None) };
        assert!(!r.apply(t(5_000)));
        assert!(!r.scene().agents.contains_key("cc:old"));
        r.apply(t(100 + LEAVE_MS + RETIRED_TTL_MS + 1));
        assert!(r.scene().agents.contains_key("cc:old"));
    }

    #[test]
    fn retired_map_is_bounded() {
        let mut r = Reducer::new();
        for i in 0..(MAX_RETIRED as u64 + 100) {
            r.retire_agent(format!("cc:r{i}"), i);
        }
        assert!(r.retired.len() <= MAX_RETIRED);
        assert!(r.retired.contains_key(&format!("cc:r{}", MAX_RETIRED as u64 + 99)));
        assert!(!r.retired.contains_key("cc:r0"));
    }

    #[test]
    fn agy_parent_not_inferred_when_two_sessions_share_cwd() {
        let mut r = Reducer::new();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        r.apply(ev(0, seen("cc:p1", Some("p1"), Some(&cwd))));
        r.apply(ev(1, seen("cc:p2", Some("p2"), Some(&format!("{}/sub", cwd.trim_end_matches('/'))))));
        r.apply(ev(30, EventKind::AgentSeen {
            id: "agy:42".into(),
            kind: AgentKind::Agy,
            label: Some("worker".into()),
            parent: None,
            cwd: Some(cwd),
            role: Some("worker".into()),
            model: None,
            slot: None,
            pid: Some(42),
            resume_id: None,
        }));
        assert!(r.scene().agents["agy:42"].parent.is_none());
    }

    #[test]
    fn usage_on_leaving_agent_does_not_revive() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:a", None, None)));
        r.apply(ev(1, EventKind::Gone { id: "cc:a".into() }));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Leaving);
        r.apply(ev(2, EventKind::Usage {
            id: "cc:a".into(),
            tokens_in: 10,
            tokens_out: 5,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            cost_usd: 0.01,
            context_pct: None,
            model: None,
        }));
        assert_eq!(r.scene().agents["cc:a"].state, AgentState::Leaving);
    }

    #[test]
    fn done_subagent_holds_then_leaves() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:p", None, None)));
        r.apply(ev(10, EventKind::AgentSeen {
            id: "cc-sub:p/1".into(),
            kind: AgentKind::ClaudeSub,
            label: Some("Explore".into()),
            parent: Some("cc:p".into()),
            cwd: None,
            role: None,
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }));
        r.apply(ev(100, EventKind::State { id: "cc-sub:p/1".into(), state: AgentState::Done, activity: Some("done".into()) }));
        r.sweep(100 + DONE_HOLD_MS - 1);
        assert_eq!(r.scene().agents["cc-sub:p/1"].state, AgentState::Done);
        r.sweep(100 + DONE_HOLD_MS);
        assert_eq!(r.scene().agents["cc-sub:p/1"].state, AgentState::Leaving);
        assert!(r.scene().agents["cc-sub:p/1"].last_event_ms >= 100 + DONE_HOLD_MS);
    }

    #[test]
    fn child_refreshes_parent_and_exempts_from_stale_and_stuck() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:p", None, None)));
        r.apply(ev(0, EventKind::State { id: "cc:p".into(), state: AgentState::Delegating, activity: None }));
        r.apply(ev(10, EventKind::AgentSeen {
            id: "cc-sub:p/1".into(),
            kind: AgentKind::ClaudeSub,
            label: Some("Explore".into()),
            parent: Some("cc:p".into()),
            cwd: None,
            role: None,
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }));
        r.apply(ev(5_000, EventKind::ToolStart { id: "cc-sub:p/1".into(), tool: "Read".into(), summary: "a.rs".into() }));
        assert_eq!(r.scene().agents["cc:p"].last_event_ms, 5_000);
        assert!(has_live_active_children(r.scene(), "cc:p"));

        // Even past STALE_MS, parent is not swept to Leaving nor alerted as stuck while child is active.
        let late = 5_000 + STALE_MS + 1_000;
        r.apply(ev(late - 500, EventKind::ToolStart { id: "cc-sub:p/1".into(), tool: "Read".into(), summary: "b.rs".into() }));
        r.sweep(late);
        assert_ne!(r.scene().agents["cc:p"].state, AgentState::Leaving);
        // The active child itself may go stale; only the delegating parent must stay unalerted.
        assert!(!r.scene().alerts.iter().any(|a| a.code == "stuck" && a.agent.as_deref() == Some("cc:p")));
    }

    #[test]
    fn ag_reconciles_into_agy_and_infers_parent() {
        let mut r = Reducer::new();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        r.apply(ev(0, seen("cc:p1", Some("p1"), Some(&cwd))));
        r.apply(ev(10, EventKind::AgentSeen {
            id: "ag:sess1".into(),
            kind: AgentKind::Agy,
            label: Some("ag".into()),
            parent: None,
            cwd: Some(cwd.clone()),
            role: None,
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }));
        r.apply(ev(20, EventKind::ToolStart { id: "ag:sess1".into(), tool: "Read".into(), summary: "x".into() }));
        r.apply(ev(30, EventKind::AgentSeen {
            id: "agy:999".into(),
            kind: AgentKind::Agy,
            label: Some("worker".into()),
            parent: None,
            cwd: Some(cwd),
            role: Some("worker".into()),
            model: Some("gemini".into()),
            slot: Some(1),
            pid: Some(999),
            resume_id: None,
        }));
        assert!(!r.scene().agents.contains_key("ag:sess1"));
        let agy = &r.scene().agents["agy:999"];
        assert_eq!(agy.parent.as_deref(), Some("cc:p1"));
        assert_eq!(agy.tool_calls, 1);
    }

    #[test]
    fn late_ag_hook_folds_into_existing_agy() {
        let mut r = Reducer::new();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let agy = |id: &str, kind: AgentKind| EventKind::AgentSeen {
            id: id.into(),
            kind,
            label: Some("x".into()),
            parent: None,
            cwd: Some(cwd.clone()),
            role: None,
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        };
        r.apply(ev(0, agy("agy:7", AgentKind::Agy)));
        r.apply(ev(10, agy("ag:late", AgentKind::Other)));
        r.apply(ev(20, EventKind::ToolStart { id: "ag:late".into(), tool: "Read".into(), summary: "x".into() }));
        assert!(!r.scene().agents.contains_key("ag:late"));
        assert_eq!(r.scene().agents["agy:7"].kind, AgentKind::Agy);
        assert_eq!(r.scene().agents["agy:7"].tool_calls, 1);
    }

    #[test]
    fn agy_call_nests_under_its_relay_subagent() {
        let mut r = Reducer::new();
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        let sub_dir = format!("{}/x", root.trim_end_matches('/'));
        let seen_full = |id: &str, kind: AgentKind, parent: Option<&str>, cwd: Option<&str>, role: Option<&str>| EventKind::AgentSeen {
            id: id.into(),
            kind,
            label: Some(id.into()),
            parent: parent.map(Into::into),
            cwd: cwd.map(Into::into),
            role: role.map(Into::into),
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        };
        // Session cwd is a subdirectory of the project root the router gets.
        r.apply(ev(0, seen_full("cc:s", AgentKind::Claude, None, Some(&sub_dir), None)));
        r.apply(ev(1, seen_full("cc-sub:s/a", AgentKind::ClaudeSub, Some("cc:s"), None, Some("agy-manager"))));
        r.apply(ev(2, seen_full("cc-sub:s/b", AgentKind::ClaudeSub, Some("cc:s"), None, Some("agy-manager"))));
        r.apply(ev(3, seen_full("cc-sub:s/c", AgentKind::ClaudeSub, Some("cc:s"), None, Some("agy-council-advisor"))));
        r.apply(ev(10, seen_full("agy:1", AgentKind::Agy, None, Some(&root), Some("manager"))));
        r.apply(ev(11, seen_full("agy:2", AgentKind::Agy, None, Some(&root), Some("manager"))));
        r.apply(ev(12, seen_full("agy:3", AgentKind::Agy, None, Some(&root), Some("council-advisor"))));
        r.apply(ev(13, seen_full("agy:4", AgentKind::Agy, None, Some(&root), Some("git"))));
        let p = |id: &str| r.scene().agents[id].parent.clone();
        assert_eq!(p("agy:1").as_deref(), Some("cc-sub:s/a"));
        assert_eq!(p("agy:2").as_deref(), Some("cc-sub:s/b"));
        assert_eq!(p("agy:3").as_deref(), Some("cc-sub:s/c"));
        // No relay subagent for this role: falls back to the session in the same tree.
        assert_eq!(p("agy:4").as_deref(), Some("cc:s"));
    }

    fn rosso_agent(id: &str, kind: AgentKind, label: &str, role: Option<&str>) -> EventKind {
        EventKind::AgentSeen {
            id: id.into(),
            kind,
            label: Some(label.into()),
            parent: None,
            cwd: None,
            role: role.map(Into::into),
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }
    }
    fn mail(from: &str, to: &[&str], ty: &str, subject: &str, escalation: bool, body: &str) -> EventKind {
        EventKind::RossoMail {
            job: "j".into(),
            from_title: from.into(),
            to_titles: to.iter().map(|s| s.to_string()).collect(),
            doc_type: ty.into(),
            verdict: Some("PASS".into()),
            subject: subject.into(),
            doc_path: None,
            escalation,
            body: body.into(),
        }
    }

    #[test]
    fn rosso_mail_records_the_job_of_each_report() {
        let mut r = Reducer::new();
        r.apply(ev(1, mail("Chief of Staff", &["Chief Architect", "Test Inspector"], "Memo", "x", false, "b")));
        let reps = &r.scene().reports;
        assert_eq!(reps.len(), 2);
        for rep in reps {
            assert_eq!(r.scene().report_jobs.get(&rep.id).map(String::as_str), Some("j"), "{rep:?}");
        }
        r.apply(ev(2, EventKind::Handoff { from: "a".into(), to: None, kind: ReportKind::Report, title: "t".into(), body: String::new(), brief: String::new() }));
        let last = r.scene().reports.last().unwrap().id;
        assert!(!r.scene().report_jobs.contains_key(&last), "plain hand-offs have no job");
    }

    #[test]
    fn rosso_mail_resolves_titles_to_agents() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(1, rosso_agent("agy:1", AgentKind::Agy, "Chief Architect\u{b7}neon-xhigh", Some("council-architect"))));
        r.apply(ev(2, rosso_agent("agy:2", AgentKind::Agy, "Correctness Seat\u{b7}neon-high", Some("council-advisor"))));
        r.apply(ev(5, mail("Chief Architect", &["Correctness Seat"], "Memo", "Review plan", false, "body")));
        let t = r.scene().reports.last().unwrap();
        assert_eq!((t.kind, t.from.as_str(), t.to.as_deref()), (ReportKind::Task, "agy:1", Some("agy:2")));
        assert_eq!((t.from_label.as_str(), t.to_label.as_str()), ("Chief Architect", "Correctness Seat"));
        assert_eq!(t.title, "Memo: Review plan [PASS]");
        r.apply(ev(6, mail("Correctness Seat", &["Chief Architect"], "Critique", "ok", false, "")));
        let rep = r.scene().reports.last().unwrap();
        assert_eq!((rep.kind, rep.to.as_deref()), (ReportKind::Report, Some("agy:1")));
        // unresolved title -> pseudo id
        r.apply(ev(7, mail("Chief Architect", &["Backend Head"], "Work Order", "x", false, "")));
        assert_eq!(r.scene().reports.last().unwrap().to.as_deref(), Some("rosso:Backend Head"));
    }

    #[test]
    fn rosso_mail_to_ceo_and_escalation() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(1, rosso_agent("agy:2", AgentKind::Agy, "Security Seat\u{b7}neon-high", Some("council-advisor"))));
        r.apply(ev(5, mail("Security Seat", &["CEO"], "Critique", "risk", false, "")));
        let t = r.scene().reports.last().unwrap();
        assert_eq!((t.kind, t.from.as_str(), t.to.as_deref(), t.to_label.as_str()), (ReportKind::Report, "agy:2", None, "CEO"));
        let n = r.scene().reports.len();
        r.apply(ev(6, mail("Security Seat", &["CEO", "Chief of Staff"], "Escalation", "ROSSO-1", true, "")));
        let reps = &r.scene().reports[n..];
        assert_eq!(reps.len(), 3);
        assert!(reps[0].title.starts_with("\u{26a0} Escalation"));
        let last = reps.last().unwrap();
        assert_eq!((last.kind, last.from.as_str(), last.to.as_deref()), (ReportKind::Result, "cc:s", None));
        // NEEDS_USER_INPUT to the CEO also sends the Chief of Staff to the boss
        let n = r.scene().reports.len();
        r.apply(ev(7, mail("Security Seat", &["CEO"], "Critique", "q", false, "NEEDS_USER_INPUT: which db?")));
        let reps = &r.scene().reports[n..];
        assert_eq!(reps.len(), 2);
        assert_eq!((reps[1].kind, reps[1].from.as_str()), (ReportKind::Result, "cc:s"));
    }

    #[test]
    fn briefs_go_down_and_reports_come_back_up() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(5, EventKind::AgentSeen {
            id: "cc-sub:s/a".into(),
            kind: AgentKind::ClaudeSub,
            label: Some("agy-manager".into()),
            parent: Some("cc:s".into()),
            cwd: None,
            role: Some("agy-manager".into()),
            model: None,
            slot: None,
            pid: None,
            resume_id: None,
        }));
        // the brief is handed down automatically when the subagent appears
        let t = &r.scene().reports[0];
        assert_eq!((t.kind, t.from.as_str(), t.to.as_deref()), (ReportKind::Task, "cc:s", Some("cc-sub:s/a")));
        assert_eq!(t.title, "(agy-manager)");
        // the report comes back to the parent (to: None = whoever asked) and fills in the brief
        r.apply(ev(9, EventKind::Handoff {
            from: "cc-sub:s/a".into(),
            to: None,
            kind: ReportKind::Report,
            title: "Backend work order".into(),
            body: "STATUS: OK".into(),
            brief: "--domain backend".into(),
        }));
        let rep = &r.scene().reports[1];
        assert_eq!(rep.to.as_deref(), Some("cc:s"));
        assert_eq!(rep.to_label, "cc:s");
        let t = &r.scene().reports[0];
        assert_eq!((t.title.as_str(), t.body.as_str()), ("Backend work order", "--domain backend"));
        // a session's result goes to the boss
        r.apply(ev(12, EventKind::Handoff {
            from: "cc:s".into(),
            to: None,
            kind: ReportKind::Result,
            title: "done".into(),
            body: "all green".into(),
            brief: String::new(),
        }));
        let res = r.scene().reports.last().unwrap();
        assert_eq!((res.to.as_deref(), res.to_label.as_str()), (None, "you"));
        // bounded
        for i in 0..(MAX_REPORTS as u64 + 20) {
            r.apply(ev(20 + i, EventKind::Handoff {
                from: "cc:s".into(), to: None, kind: ReportKind::Result, title: "x".into(), body: String::new(), brief: String::new(),
            }));
        }
        assert_eq!(r.scene().reports.len(), MAX_REPORTS);
    }

    #[test]
    fn duplicate_claude_sub_labels_disambiguated() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:p", None, None)));
        for (i, id) in ["cc-sub:p/1", "cc-sub:p/2"].iter().enumerate() {
            r.apply(ev(i as u64 + 1, EventKind::AgentSeen {
                id: (*id).into(),
                kind: AgentKind::ClaudeSub,
                label: Some("Explore".into()),
                parent: Some("cc:p".into()),
                cwd: None,
                role: None,
                model: None,
                slot: None,
                pid: None,
                resume_id: None,
            }));
        }
        assert_eq!(r.scene().agents["cc-sub:p/1"].label, "Explore");
        assert_eq!(r.scene().agents["cc-sub:p/2"].label, "Explore#2");
    }

    fn mail_v(job: &str, from: &str, to: &[&str], ty: &str, verdict: Option<&str>) -> EventKind {
        EventKind::RossoMail {
            job: job.into(),
            from_title: from.into(),
            to_titles: to.iter().map(|s| s.to_string()).collect(),
            doc_type: ty.into(),
            verdict: verdict.map(Into::into),
            subject: "s".into(),
            doc_path: None,
            escalation: false,
            body: String::new(),
        }
    }

    #[test]
    fn guard_denials_are_tallied_and_bounded() {
        let mut r = Reducer::new();
        let d = |reason: &str, deny| EventKind::GuardDecision { agent: "agy-tester".into(), tool: "Bash".into(), reason: reason.into(), deny };
        assert!(r.apply(ev(100, d("not on allowlist", true))));
        assert!(!r.apply(ev(101, d("ignored", false))));
        r.apply(ev(200, d("not on allowlist", true)));
        let g = &r.scene().guard;
        assert_eq!((g.deny_total, g.last_deny_ms, g.by_reason["not on allowlist"]), (2, 200, 2));
        assert_eq!(g.last_agent, "agy-tester");
        for i in 0..100 {
            r.apply(ev(300 + i, d(&format!("reason {i}"), true)));
        }
        assert!(r.scene().guard.by_reason.len() <= crate::model::MAX_GUARD_REASONS);
        assert_eq!(r.scene().guard.deny_total, 102);
    }

    #[test]
    fn verdicts_from_audit_and_rosso_mail_and_pruned_on_removal() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(1, rosso_agent("agy:2", AgentKind::Agy, "Correctness Seat\u{b7}neon-high", Some("council-advisor"))));
        r.apply(ev(2, EventKind::Verdict { id: "agy:2".into(), verdict: "approve".into() }));
        assert_eq!(r.scene().verdicts["agy:2"], "APPROVE");
        assert!(!r.apply(ev(3, EventKind::Verdict { id: "agy:2".into(), verdict: "APPROVE".into() })));
        assert!(!r.apply(ev(3, EventKind::Verdict { id: "agy:nope".into(), verdict: "PASS".into() })));
        r.apply(ev(4, mail_v("j", "Correctness Seat", &["Chief of Staff"], "Critique", Some("OBJECT"))));
        assert_eq!(r.scene().verdicts["agy:2"], "OBJECT");
        r.apply(ev(5, mail_v("j", "Correctness Seat", &["Chief of Staff"], "Critique", None)));
        assert_eq!(r.scene().verdicts["agy:2"], "OBJECT", "no verdict keeps the last one");
        r.apply(ev(10, EventKind::Gone { id: "agy:2".into() }));
        r.sweep(10 + LEAVE_MS + 10);
        r.sweep(10 + 2 * LEAVE_MS + 20);
        assert!(!r.scene().agents.contains_key("agy:2"));
        assert!(r.scene().verdicts.is_empty());
    }

    #[test]
    fn failure_entrypoint_intray_and_router_extra() {
        use crate::model::{FailureInfo, RouterExtra};
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(1, rosso_agent("agy:1", AgentKind::Agy, "Chief Inspector\u{b7}neon-medium", Some("supervisor"))));
        let info = FailureInfo { at_ms: 5, outcome: "ERROR/TIMEOUT".into(), diagnosis: "STALLED".into(), hint: "stalled: retry once".into() };
        assert!(r.apply(ev(2, EventKind::Failure { id: "agy:1".into(), info: info.clone() })));
        assert_eq!(r.scene().failures["agy:1"], info);
        assert!(!r.apply(ev(2, EventKind::Failure { id: "agy:gone".into(), info })));
        assert!(r.apply(ev(3, EventKind::Entrypoint { id: "cc:s".into(), entrypoint: "claude-desktop".into() })));
        assert!(!r.apply(ev(4, EventKind::Entrypoint { id: "cc:s".into(), entrypoint: "claude-desktop".into() })));
        assert!(!r.apply(ev(4, EventKind::Entrypoint { id: "cc:zzz".into(), entrypoint: "cli".into() })));
        assert_eq!(r.scene().entrypoints["cc:s"], "claude-desktop");
        assert!(r.apply(ev(5, EventKind::InTray { slug: "chief-inspector".into(), count: 3 })));
        assert_eq!(r.scene().in_trays["agy:1"], 3);
        assert!(!r.apply(ev(5, EventKind::InTray { slug: "chief-inspector".into(), count: 3 })));
        assert!(!r.apply(ev(5, EventKind::InTray { slug: "nobody".into(), count: 3 })));
        assert!(r.apply(ev(6, EventKind::InTray { slug: "chief-inspector".into(), count: 0 })));
        assert!(r.scene().in_trays.is_empty());
        let x = RouterExtra { orphans: 2, reserved_mb: Some(10), would_start: Some(false), role_models: Default::default() };
        assert!(r.apply(ev(7, EventKind::RouterExtra(x.clone()))));
        assert!(!r.apply(ev(8, EventKind::RouterExtra(x))));
        assert_eq!(r.scene().router_extra.orphans, 2);
    }

    #[test]
    fn document_types_are_kept_and_council_round_follows_the_latest_job() {
        let mut r = Reducer::new();
        r.apply(ev(0, seen("cc:s", None, None)));
        r.apply(ev(1, rosso_agent("agy:1", AgentKind::Agy, "Chief Architect\u{b7}x", Some("council-architect"))));
        r.apply(ev(2, rosso_agent("agy:2", AgentKind::Agy, "Security Seat\u{b7}x", Some("council-advisor"))));
        r.apply(ev(3, mail_v("old", "Chief Architect", &["Security Seat"], "Task Card", None)));
        r.apply(ev(4, mail_v("old", "Security Seat", &["Chief Architect"], "Critique", Some("REVISE"))));
        r.apply(ev(5, mail_v("new", "Chief Architect", &["Security Seat"], "Task Card", None)));
        let last = r.scene().reports.last().unwrap().id;
        assert_eq!(r.scene().report_types[&last], "Task Card");
        let round = crate::rosso::council_round(r.scene()).unwrap();
        assert_eq!((round.job.as_str(), round.reached, round.current()), ("new", [true, false, false, false], 0));
        r.apply(ev(6, mail_v("new", "Security Seat", &["Chief Architect"], "Critique", Some("OBJECT"))));
        r.apply(ev(7, mail_v("new", "Chief Architect", &["Security Seat"], "Rebuttal", None)));
        r.apply(ev(8, mail_v("new", "Chief of Staff", &["CEO"], "Memo", None)));
        let round = crate::rosso::council_round(r.scene()).unwrap();
        assert_eq!((round.reached, round.current()), ([true, true, true, false], 2));
        r.apply(ev(9, mail_v("new", "Chief Architect", &["CEO"], "Decision Record", None)));
        assert_eq!(crate::rosso::council_round(r.scene()).unwrap().current(), 3);
        assert_eq!(crate::rosso::council_round(&Scene::default()), None);
        assert_eq!(crate::rosso::slug("Client Intent Seat"), "client-intent-seat");
        assert_eq!(crate::rosso::verdict_class("APPROVE_WITH_CONDITIONS"), 1);
        assert_eq!(crate::rosso::verdict_class("OBJECT"), 2);
        assert_eq!(crate::rosso::verdict_class("SUPPORT"), 0);
    }
}
