//! office-core: the shared contract for the agent office.
//! Sources produce `Event`s; the `Reducer` folds them into a `Scene`; the TUI renders the `Scene`.
//! This file is the CONTRACT between parallel workstreams: change it only by agreement.

pub mod model;
pub mod event;
pub mod reducer;
pub mod alerts;
pub mod config;
pub mod paths;

pub use event::{Event, EventKind, SourceKind};
pub use model::{Agent, AgentId, AgentState, Alert, AlertLevel, RouterStatus, Scene};
pub use reducer::Reducer;
