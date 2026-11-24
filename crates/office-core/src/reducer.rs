use crate::event::Event;
use crate::model::Scene;

/// Folds Events into the Scene. Signatures are part of the contract; bodies are owned by workstream A.
#[derive(Default)]
pub struct Reducer {
    scene: Scene,
}

impl Reducer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event. Returns true if the scene changed.
    pub fn apply(&mut self, _ev: Event) -> bool {
        todo!("workstream A")
    }

    /// Periodic housekeeping: Arriving->Idle after walk-in, stale/Gone agents -> Leaving -> removed,
    /// stuck detection hand-off to alerts. Called ~every 500 ms.
    pub fn sweep(&mut self, _now_ms: u64) -> bool {
        todo!("workstream A")
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
}
