//! Workstream A. Background producers of office_core::Event.
use office_core::{config::Config, Event};
use std::sync::mpsc::Sender;

pub mod control;

/// Start every source on its own thread(s): hook socket listener, Claude transcript tailer, agy router scanner.
/// Must never panic the caller; source failures become Alert events.
pub fn spawn_all(_cfg: &Config, _tx: Sender<Event>) {
    todo!("workstream A")
}
