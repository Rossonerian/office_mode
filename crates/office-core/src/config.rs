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
    /// The office cat (ambient pet, not an agent).
    pub cat: CatConfig,
    /// The CEO (you), at the big desk in the top-right corner.
    pub ceo: CeoConfig,
}

/// `[cat] enabled = true, name = "Pixel", color = "grey"|"orange"|"black"|"white"|"calico"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatConfig {
    pub enabled: bool,
    pub name: String,
    pub color: String,
}

impl Default for CatConfig {
    fn default() -> Self {
        CatConfig { enabled: true, name: "Pixel".into(), color: "grey".into() }
    }
}

/// `[ceo] enabled = true, name = "CEO", number = 9, skin = "#c68642", hair = "black",
/// hair_style = "short"|"curly"|"long"|"bald"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CeoConfig {
    pub enabled: bool,
    pub name: String,
    /// Shirt number on the back of the jersey (0 = none).
    pub number: u32,
    /// "#rrggbb".
    pub skin: String,
    /// A colour name (black, brown, blonde, red, grey, white) or "#rrggbb".
    pub hair: String,
    pub hair_style: String,
}

impl Default for CeoConfig {
    fn default() -> Self {
        CeoConfig {
            enabled: true,
            name: "CEO".into(),
            number: 9,
            skin: "#c68642".into(),
            hair: "black".into(),
            hair_style: "short".into(),
        }
    }
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
            cat: CatConfig::default(),
            ceo: CeoConfig::default(),
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

    /// Load a specific config file (defaults on any error).
    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    /// $/1M (input, output) for a model id: the longest `prices` key that is a substring of it.
    pub fn price_for(&self, model: &str) -> Option<(f64, f64)> {
        let m = model.to_ascii_lowercase();
        self.prices
            .iter()
            .filter(|(k, _)| m.contains(&k.to_ascii_lowercase()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, v)| *v)
    }

    /// Estimated $ for one API call: cache writes at 125% and cache reads at 10% of the input price.
    pub fn cost_usd(&self, model: &str, input: u64, output: u64, cache_write: u64, cache_read: u64) -> f64 {
        let Some((pi, po)) = self.price_for(model) else { return 0.0 };
        (input as f64 * pi + output as f64 * po + cache_write as f64 * pi * 1.25 + cache_read as f64 * pi * 0.10) / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prices_match_model_substrings() {
        let c = Config::default();
        assert_eq!(c.price_for("claude-opus-5-5"), Some((15.0, 75.0)));
        assert_eq!(c.price_for("claude-haiku-5-5"), Some((0.8, 4.0)));
        assert_eq!(c.price_for("gpt"), None);
        let cost = c.cost_usd("claude-sonnet-4", 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((cost - (3.0 + 15.0 + 3.75 + 0.3)).abs() < 1e-9);
    }
    #[test]
    fn partial_toml_uses_defaults() {
        let c: Config = toml::from_str("permission_timeout_s = 30\n").unwrap();
        assert_eq!(c.permission_timeout_s, 30);
        assert_eq!(c.fps, 12);
        assert_eq!(c.cat, CatConfig::default());
        assert_eq!(c.ceo, CeoConfig::default());
    }
    #[test]
    fn cat_and_ceo_sections_parse() {
        let c: Config = toml::from_str(
            "[cat]\nname = \"Mochi\"\ncolor = \"calico\"\n\n[ceo]\nnumber = 10\nskin = \"#8d5524\"\nhair_style = \"curly\"\n",
        )
        .unwrap();
        assert!(c.cat.enabled);
        assert_eq!((c.cat.name.as_str(), c.cat.color.as_str()), ("Mochi", "calico"));
        assert!(c.ceo.enabled);
        assert_eq!(c.ceo.name, "CEO");
        assert_eq!(c.ceo.number, 10);
        assert_eq!(c.ceo.skin, "#8d5524");
        assert_eq!(c.ceo.hair, "black");
        assert_eq!(c.ceo.hair_style, "curly");
        let off: Config = toml::from_str("[cat]\nenabled = false\n[ceo]\nenabled = false\n").unwrap();
        assert!(!off.cat.enabled && !off.ceo.enabled);
        assert_eq!(off.cat.name, "Pixel");
        let d = Config::default();
        assert_eq!((d.ceo.skin.as_str(), d.ceo.hair.as_str(), d.ceo.hair_style.as_str(), d.ceo.number), ("#c68642", "black", "short", 9));
        assert_eq!(d.cat.color, "grey");
    }
}
