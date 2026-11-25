use crate::model::{AgentId, AgentKind, AgentState, Alert, ReportKind, RouterStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// Claude Code hook (via office-hook over the unix socket).
    ClaudeHook,
    /// Claude Code transcript tail (~/.claude/projects/**.jsonl): tokens/cost/context.
    ClaudeTranscript,
    /// Antigravity CLI hook (via office-hook, OFFICE_SOURCE=antigravity).
    AgyHook,
    /// agy-route: process scan, lock files, audit log, dry-run capacity.
    AgyRouter,
    /// ROSSO job logbook / mailroom tail.
    Rosso,
    /// Synthetic (tests / demo mode).
    Demo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: u64,
    pub source: SourceKind,
    pub kind: EventKind,
}

/// Everything a source can tell the reducer. Sources never mutate the Scene directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    /// Upsert identity/metadata. Fields that are None leave existing values untouched.
    AgentSeen {
        id: AgentId,
        kind: AgentKind,
        label: Option<String>,
        parent: Option<AgentId>,
        cwd: Option<String>,
        role: Option<String>,
        model: Option<String>,
        slot: Option<u32>,
        pid: Option<u32>,
        resume_id: Option<String>,
    },
    State { id: AgentId, state: AgentState, activity: Option<String> },
    ToolStart { id: AgentId, tool: String, summary: String },
    ToolEnd { id: AgentId, tool: String, ok: bool },
    Usage {
        id: AgentId,
        /// Fresh (uncached) input tokens.
        tokens_in: u64,
        tokens_out: u64,
        #[serde(default)]
        tokens_cache_read: u64,
        #[serde(default)]
        tokens_cache_write: u64,
        cost_usd: f64,
        context_pct: Option<f32>,
        #[serde(default)]
        model: Option<String>,
    },
    Resources { id: AgentId, rss_mb: u32 },
    PermissionRequested { id: AgentId, request_id: String, summary: String },
    PermissionResolved { id: AgentId, request_id: String },
    Gone { id: AgentId },
    Router(RouterStatus),
    Alert(Alert),
    /// A document handed from one agent to another. `to: None` means "whoever asked": the
    /// sender's parent, or you (the boss) for a top-level session.
    Handoff {
        from: AgentId,
        to: Option<AgentId>,
        kind: ReportKind,
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        brief: String,
    },
    /// A ROSSO delivery (logbook line) or escalation (mailroom line), by office title.
    RossoMail {
        job: String,
        from_title: String,
        to_titles: Vec<String>,
        doc_type: String,
        verdict: Option<String>,
        subject: String,
        doc_path: Option<String>,
        escalation: bool,
        #[serde(default)]
        body: String,
    },
}

impl Event {
    pub fn now(source: SourceKind, kind: EventKind) -> Self {
        Event { at_ms: crate::paths::now_ms(), source, kind }
    }
}
