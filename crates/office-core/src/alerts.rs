use crate::event::Event;
use crate::model::{Alert, Scene};

/// Stateful detectors (loop, stuck, failure streak, needs_you, cost). Owned by workstream A.
#[derive(Default)]
pub struct Detector {}

impl Detector {
    pub fn new(_cfg: &crate::config::Config) -> Self {
        Self::default()
    }
    /// Inspect an event before it's applied; return any alerts it triggers.
    pub fn observe(&mut self, _ev: &Event, _scene: &Scene) -> Vec<Alert> {
        todo!("workstream A")
    }
    /// Time-based checks (stuck). Called from Reducer::sweep.
    pub fn tick(&mut self, _scene: &Scene, _now_ms: u64) -> Vec<Alert> {
        todo!("workstream A")
    }
}
