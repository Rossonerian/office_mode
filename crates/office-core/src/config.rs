use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Theme mix: each layer can come from a different theme pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeMix {
    pub floor: String,
    pub walls: String,
    pub furniture: String,
    pub decor: String,
    pub characters: String,
}

impl Default for ThemeMix {
    fn default() -> Self {
        let p = "procedural".to_string();
        ThemeMix { floor: p.clone(), walls: p.clone(), furniture: p.clone(), decor: p.clone(), characters: p }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeMix,
    /// Named presets the `t` key cycles through (name -> mix).
    pub presets: BTreeMap<String, ThemeMix>,
    pub fps: u32,
    pub notifications: bool,
    pub cost_alert_usd: f64,
    pub stuck_after_s: u64,
    /// $ per 1M tokens: model-substring -> (input, output).
    pub prices: BTreeMap<String, (f64, f64)>,
    /// agy role -> model (fallback when the audit log hasn't shown it yet).
    pub agy_models: BTreeMap<String, String>,
    pub agy_route: String,
    /// 0 = approve/deny from the office disabled.
    pub permission_timeout_s: u64,
    pub sidebar: bool,
}

impl Default for Config {
    fn default() -> Self {
        let mut prices = BTreeMap::new();
        prices.insert("opus".into(), (15.0, 75.0));
        prices.insert("sonnet".into(), (3.0, 15.0));
        prices.insert("haiku".into(), (0.8, 4.0));
        let mut agy = BTreeMap::new();
        for (r, m) in [
            ("architect", "gemini-4-neon-high"),
            ("supervisor", "gemini-4-neon-medium"),
            ("worker", "gemini-4-hydrogen-low"),
            ("tester", "gemini-4-hydrogen-high"),
            ("git", "gemini-4-hydrogen-low"),
            ("manager", "gemini-4-hydrogen-medium"),
            ("council-architect", "gemini-4-neon-xhigh"),
            ("council-advisor", "gemini-4-neon-high"),
            ("fallback", "gemini-3.8-flash-medium"),
        ] {
            agy.insert(r.into(), m.into());
        }
        Config {
            theme: ThemeMix::default(),
            presets: BTreeMap::new(),
            fps: 12,
            notifications: true,
            cost_alert_usd: 5.0,
            stuck_after_s: 120,
            prices,
            agy_models: agy,
            agy_route: dirs::home_dir().unwrap_or_default().join(".claude/bin/agy-route").to_string_lossy().into(),
            permission_timeout_s: 0,
            sidebar: true,
        }
    }
}

impl Config {
    /// Load ~/.config/agent-office/config.toml, falling back to defaults (never fails hard).
    pub fn load() -> Self {
        std::fs::read_to_string(crate::paths::config_file())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}
