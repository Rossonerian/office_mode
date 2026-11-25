use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable id: "<source>:<session or call id>", e.g. "cc:3f2a…", "cc-sub:3f2a…/toolu_01…", "agy:12698".
pub type AgentId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    /// A top-level Claude Code session.
    Claude,
    /// A Claude Code subagent (Task/Agent tool); `parent` is set.
    ClaudeSub,
    /// An agy (Antigravity CLI) call routed through ~/.claude/bin/agy-route; `role`/`model`/`slot` set.
    Agy,
    /// Anything else that speaks the hook protocol.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Arriving,
    Idle,
    Thinking,
    /// Editing / writing files.
    Typing,
    /// Read / Grep / Glob / WebFetch.
    Reading,
    /// Bash / shell / long tool.
    Running,
    /// Spawned subagents and waiting on them.
    Delegating,
    /// Waiting for the human (permission prompt, AskUserQuestion, NEEDS_USER_INPUT).
    NeedsYou,
    /// Queued by the router (no slot / low RAM).
    Queued,
    Done,
    Error,
    Leaving,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub kind: AgentKind,
    /// Short display label, e.g. "cc·DICE", "agy-worker", "advisor·neon-high".
    pub label: String,
    pub parent: Option<AgentId>,
    pub cwd: Option<String>,
    /// agy role (worker, council-architect, …) or Claude subagent_type.
    pub role: Option<String>,
    pub model: Option<String>,
    pub slot: Option<u32>,
    pub pid: Option<u32>,
    pub state: AgentState,
    /// Short human text for the speech bubble, e.g. "Edit main.py".
    pub activity: String,
    pub started_ms: u64,
    pub last_event_ms: u64,
    /// Fresh (uncached) input tokens.
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Input tokens served from the prompt cache (cheap; not shown in totals).
    #[serde(default)]
    pub tokens_cache_read: u64,
    /// Input tokens written to the prompt cache.
    #[serde(default)]
    pub tokens_cache_write: u64,
    pub cost_usd: f64,
    pub context_pct: Option<f32>,
    pub rss_mb: Option<u32>,
    pub tool_calls: u32,
    pub tool_failures: u32,
    #[serde(default)]
    pub current_tool: Option<String>,
    #[serde(default)]
    pub last_tool: Option<String>,
    #[serde(default)]
    pub last_tool_ok: Option<bool>,
    /// Session id usable for `claude --resume`.
    pub resume_id: Option<String>,
    /// Pending Claude permission request id (control action: approve/deny).
    pub pending_permission: Option<String>,
}

impl Agent {
    /// Tokens for the sidebar/header: input + cache writes + output, without cache reads (which
    /// dominate long sessions while costing a tenth of fresh input).
    pub fn tokens_shown(&self) -> u64 {
        self.tokens_in + self.tokens_cache_write + self.tokens_out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertLevel {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub at_ms: u64,
    pub level: AlertLevel,
    pub agent: Option<AgentId>,
    /// e.g. "loop", "stuck", "tool_denied", "router_busy", "timeout", "needs_you", "cost".
    pub code: String,
    pub message: String,
}

/// Live state of ~/.claude/bin/agy-route (from its dry-run + lock files + audit log).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouterStatus {
    pub capacity: Option<u32>,
    pub max_concurrent: Option<u32>,
    pub busy_slots: u32,
    pub free_mb: Option<u32>,
    pub per_proc_mb: Option<u32>,
    pub pressure: Option<u8>,
    pub queued: u32,
    pub limit_text: Option<String>,
}

/// What a hand-off carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportKind {
    /// Work handed down: a session briefs a subagent, a relay subagent starts an agy call.
    Task,
    /// Work handed back up: a subagent's or agy call's final report to whoever asked.
    Report,
    /// A top-level session's turn output, taken to you (the boss).
    Result,
}

/// One document passed between agents (or to you). Labels are captured at hand-off time,
/// so the record stays readable after the agents have left.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub id: u64,
    pub at_ms: u64,
    pub kind: ReportKind,
    pub from: AgentId,
    pub from_label: String,
    /// None = the boss (you).
    pub to: Option<AgentId>,
    pub to_label: String,
    pub title: String,
    /// The report text (truncated to a bounded size).
    pub body: String,
    /// The brief the sender was given (for subagent reports), if known.
    #[serde(default)]
    pub brief: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scene {
    pub agents: BTreeMap<AgentId, Agent>,
    /// Hand-offs between agents and to you, oldest first (bounded).
    #[serde(default)]
    pub reports: Vec<Report>,
    /// ROSSO job name per report id (only for documents that came from a job's logbook/mailroom).
    #[serde(default)]
    pub report_jobs: BTreeMap<u64, String>,
    /// ROSSO document type per report id (Critique, Rebuttal, Decision Record, ...).
    #[serde(default)]
    pub report_types: BTreeMap<u64, String>,
    /// Last verdict (APPROVE, PASS, REVISE, OBJECT, ...) per agent.
    #[serde(default)]
    pub verdicts: BTreeMap<AgentId, String>,
    /// Last failed agy call per agent (diagnosis + plain hint).
    #[serde(default)]
    pub failures: BTreeMap<AgentId, FailureInfo>,
    /// Claude session entrypoint ("claude-desktop" | "cli") per `cc:<sid>` agent.
    #[serde(default)]
    pub entrypoints: BTreeMap<AgentId, String>,
    /// Documents waiting in an agent's ROSSO in-tray.
    #[serde(default)]
    pub in_trays: BTreeMap<AgentId, u32>,
    /// Security Desk (agy-guard) tallies.
    #[serde(default)]
    pub guard: GuardStats,
    /// Extra agy-route --status fields (orphans, reserved RAM, role models).
    #[serde(default)]
    pub router_extra: RouterExtra,
    pub alerts: Vec<Alert>,
    pub router: RouterStatus,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub now_ms: u64,
}

/// A failed agy call, read from `~/.claude/run/failures/<run>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FailureInfo {
    pub at_ms: u64,
    /// "ERROR" | "TIMEOUT" | other router outcome.
    pub outcome: String,
    /// "STALLED" | "ACTIVE_TOO_BIG" | "" (not diagnosed).
    pub diagnosis: String,
    /// Plain hint, e.g. "stalled: retry once".
    pub hint: String,
}

/// Security Desk counters (bounded: at most `MAX_GUARD_REASONS` distinct reasons).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GuardStats {
    pub deny_total: u32,
    pub by_reason: BTreeMap<String, u32>,
    /// Event time of the last deny (0 = none yet).
    pub last_deny_ms: u64,
    pub last_agent: String,
    pub last_tool: String,
}

pub const MAX_GUARD_REASONS: usize = 24;

/// `agy-route --status` fields beyond `RouterStatus`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouterExtra {
    /// Leftover agy processes with no router (ORPHAN_AGY).
    pub orphans: u32,
    pub reserved_mb: Option<u32>,
    pub would_start: Option<bool>,
    /// role -> model (ROLE_MODELS).
    pub role_models: BTreeMap<String, String>,
}
