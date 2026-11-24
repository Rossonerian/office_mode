//! Workstream B. Theme loading + scene compositing into an RGBA framebuffer.
use office_core::{config::{Config, ThemeMix}, AgentId, Scene};

pub struct Renderer {}

impl Renderer {
    pub fn new(_cfg: &Config) -> Self {
        todo!("workstream B")
    }
    /// All available theme names (always includes "procedural").
    pub fn themes(&self) -> Vec<String> {
        todo!("workstream B")
    }
    pub fn set_mix(&mut self, _mix: &ThemeMix) {
        todo!("workstream B")
    }
    pub fn mix(&self) -> &ThemeMix {
        todo!("workstream B")
    }
    /// Compose the office for `scene` at time `t_ms` into a px_w × px_h image.
    /// `selected` gets a highlight ring.
    pub fn compose(&mut self, _scene: &Scene, _selected: Option<&AgentId>, _px_w: u32, _px_h: u32, _t_ms: u64) -> image::RgbaImage {
        todo!("workstream B")
    }
    /// Which agent (if any) is drawn at pixel (x, y) in the last composed frame (for mouse clicks).
    pub fn hit_test(&self, _x: u32, _y: u32) -> Option<AgentId> {
        todo!("workstream B")
    }
}
