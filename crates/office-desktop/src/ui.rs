//! Desktop UI state, pure coordinate & drag-and-drop helpers, and `egui` rendering.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Rounding, Stroke, TextureOptions, Vec2};
use office::app::chime::Chime;
use office::app::fmt;
use office::app::state::{idle_kill_candidates, persist_config, report_list_for, PRESET_ORDER};
use office::render::{CAT_ID, CEO_ID, WHITEBOARD_ID};
use office_core::assets::{
    import_ceo_outfit, import_sound, import_sprite_override, import_theme_pack, list_outfits, sanitize_pack_name,
    SOUND_SLOTS, SPRITE_NAMES,
};
use office_core::config::{CeoConfig, Config, ShowConfig, SoundsConfig, ThemeMix};
use office_core::model::{Agent, AgentId, AgentKind, AgentState, AlertLevel, ReportKind, Scene};
use office_core::paths;
use office_core::whiteboard::{TaskSource, Whiteboard};
use std::path::{Path, PathBuf};

/// Toast auto-expiration duration in milliseconds.
pub const TOAST_DURATION_MS: u64 = 4_500;

/// Active tab in the right docked panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockTab {
    Agents,
    Whiteboard,
    CeoOutfit,
    Inbox,
}

/// Asset import kind supported by toolbar file pickers and drag-and-drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    ThemePack,
    CeoOutfit,
    SpriteOverride,
    SoundWav,
}

impl ImportKind {
    pub fn label(self) -> &'static str {
        match self {
            ImportKind::ThemePack => "Theme Pack (folder)",
            ImportKind::CeoOutfit => "CEO Outfit",
            ImportKind::SpriteOverride => "Sprite Override (PNG)",
            ImportKind::SoundWav => "Sound WAV",
        }
    }
}

/// Result of classifying a dropped file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropClassification {
    pub default_kind: ImportKind,
    pub allowed_kinds: Vec<ImportKind>,
    pub suggested_name: String,
    pub is_dir: bool,
}

/// State of the interactive asset import confirmation modal (SEC-7).
#[derive(Debug, Clone)]
pub struct ImportModalState {
    pub source_path: PathBuf,
    pub kind: ImportKind,
    pub allowed_kinds: Vec<ImportKind>,
    pub pack_name: String,
    pub outfit_mode_full: bool,
    pub sprite_name: String,
    pub sound_slot: String,
    pub confirm_overwrite: bool,
    pub error: Option<String>,
}

/// Non-fatal dismissible banner displayed above the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub id: u64,
    pub level: AlertLevel,
    pub message: String,
}

/// Auto-expiring status toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub level: AlertLevel,
    pub message: String,
    pub until_ms: u64,
}

/// Pending side-effecting action triggered from the UI, handled by `DesktopApp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    ApprovePermission { id: AgentId, request_id: String },
    DenyPermission { id: AgentId, request_id: String },
    CancelAgent { id: AgentId, pid: u32 },
    ResumeAgent { id: AgentId },
    KillIdleAgents,
    ReapOrphans,
    ApplyThemeMix,
    ReloadThemes,
    ApplyCeo,
    SyncWhiteboardToRenderer,
    ToggleSound(bool),
    ToggleNotifications(bool),
    SoundSlotUpdated(SoundsConfig),
    RefreshOutfits,
}

/// Pure, unit-testable coordinate mapping from `egui` logical pointer coordinates inside the
/// drawn image rectangle `(min_x, min_y, width, height)` into 1× logical pixel coordinates
/// `(0..logical_w, 0..logical_h)` for the pixel-art office canvas.
pub fn pointer_to_logical(
    pointer_pos: (f32, f32),
    img_rect: (f32, f32, f32, f32),
    logical_size: (u32, u32),
) -> Option<(u32, u32)> {
    let (px, py) = pointer_pos;
    let (rx, ry, rw, rh) = img_rect;
    let (lw, lh) = logical_size;
    if rw <= 0.0 || rh <= 0.0 || lw == 0 || lh == 0 {
        return None;
    }
    if px < rx || py < ry || px >= rx + rw || py >= ry + rh {
        return None;
    }
    let nx = (px - rx) / rw;
    let ny = (py - ry) / rh;
    let lx = ((nx * lw as f32).floor() as u32).min(lw - 1);
    let ly = ((ny * lh as f32).floor() as u32).min(lh - 1);
    Some((lx, ly))
}

/// Compute a centered, aspect-ratio-preserving rectangle `(x, y, w, h)` for `logical_size`
/// inside an available viewport rectangle `(ax, ay, aw, ah)`.
pub fn aspect_fit_rect(avail: (f32, f32, f32, f32), logical_size: (u32, u32)) -> (f32, f32, f32, f32) {
    let (ax, ay, aw, ah) = avail;
    let (lw, lh) = logical_size;
    if aw <= 0.0 || ah <= 0.0 || lw == 0 || lh == 0 {
        return (ax, ay, 0.0, 0.0);
    }
    let scale = (aw / lw as f32).min(ah / lh as f32);
    let dw = (lw as f32 * scale).max(1.0);
    let dh = (lh as f32 * scale).max(1.0);
    let dx = ax + (aw - dw) * 0.5;
    let dy = ay + (ah - dh) * 0.5;
    (dx, dy, dw, dh)
}

/// Convert logical pixel coordinates `(lx, ly)` into output-frame coordinates `(fx, fy)`
/// matching `office::render::Renderer::hit_test(fx, fy)` given the `LogicalFrame` view `(ox, oy, scale)`.
pub fn logical_to_hit_coords(logical: (u32, u32), ox: u32, oy: u32, scale: f32) -> (u32, u32) {
    let s = scale.max(0.001);
    let fx = ((logical.0 as f32 + 0.5) * s + ox as f32).floor().max(0.0) as u32;
    let fy = ((logical.1 as f32 + 0.5) * s + oy as f32).floor().max(0.0) as u32;
    (fx, fy)
}

/// Suggest a sanitized pack name (`^[a-z0-9][a-z0-9_-]{0,47}$`) from a file or folder path.
pub fn suggest_pack_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("custom");
    let mut out = String::new();
    for ch in stem.chars() {
        let low = ch.to_ascii_lowercase();
        if low.is_ascii_lowercase() || low.is_ascii_digit() {
            out.push(low);
        } else if (low == '-' || low == '_' || low == ' ' || low == '.') && !out.is_empty() {
            let mapped = if low == '_' { '_' } else { '-' };
            if !out.ends_with(mapped) {
                out.push(mapped);
            }
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '_');
    if trimmed.is_empty() {
        "custom".into()
    } else {
        trimmed.to_string()
    }
}

/// Classify a dropped path into allowed import targets. Returns `None` for unsupported files.
pub fn classify_dropped_path(path: &Path) -> Option<DropClassification> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let suggested_name = suggest_pack_name(path);
    if meta.is_dir() {
        let has_outfit = path.join("outfit.toml").is_file();
        let has_theme = path.join("theme.toml").is_file();
        let default_kind = if has_outfit && !has_theme {
            ImportKind::CeoOutfit
        } else {
            ImportKind::ThemePack
        };
        return Some(DropClassification {
            default_kind,
            allowed_kinds: vec![ImportKind::ThemePack, ImportKind::CeoOutfit],
            suggested_name,
            is_dir: true,
        });
    }
    if meta.is_file() {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        return match ext.as_deref() {
            Some("png") => Some(DropClassification {
                default_kind: ImportKind::CeoOutfit,
                allowed_kinds: vec![ImportKind::CeoOutfit, ImportKind::SpriteOverride],
                suggested_name,
                is_dir: false,
            }),
            Some("wav") => Some(DropClassification {
                default_kind: ImportKind::SoundWav,
                allowed_kinds: vec![ImportKind::SoundWav],
                suggested_name,
                is_dir: false,
            }),
            _ => None,
        };
    }
    None
}

/// Check whether importing with the chosen kind and target name would overwrite an existing
/// pack or file inside `themes_dir` / `outfits_dir` / `sounds_dir`.
pub fn check_import_overwrite(
    kind: ImportKind,
    pack_name: &str,
    sprite_name: &str,
    sound_slot: &str,
    themes_dir: &Path,
    outfits_dir: &Path,
    sounds_dir: &Path,
) -> Option<PathBuf> {
    match kind {
        ImportKind::ThemePack => {
            let clean = sanitize_pack_name(pack_name).ok()?;
            let dest = themes_dir.join(clean);
            dest.exists().then_some(dest)
        }
        ImportKind::CeoOutfit => {
            let clean = sanitize_pack_name(pack_name).ok()?;
            let dest = outfits_dir.join(clean);
            dest.exists().then_some(dest)
        }
        ImportKind::SpriteOverride => {
            let clean = sanitize_pack_name(pack_name).ok()?;
            if !SPRITE_NAMES.contains(&sprite_name) {
                return None;
            }
            let dest = themes_dir.join(clean).join("sprites").join(format!("{sprite_name}.png"));
            dest.exists().then_some(dest)
        }
        ImportKind::SoundWav => {
            if !SOUND_SLOTS.contains(&sound_slot) {
                return None;
            }
            let dest = sounds_dir.join(format!("{sound_slot}.wav"));
            dest.exists().then_some(dest)
        }
    }
}

/// Validate an optional `#rrggbb` hex string (empty -> `Ok(None)`).
pub fn validate_optional_hex(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid hex color {trimmed:?} (expected #rrggbb)"));
    }
    Ok(Some(format!("#{}", hex.to_ascii_lowercase())))
}

/// Parse `#rrggbb` into `Color32` for preview swatches.
fn parse_hex_color32(raw: &str) -> Option<Color32> {
    let hex = raw.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// Order discovered themes with built-in `PRESET_ORDER` first, then custom packs sorted alphabetically.
pub fn ordered_themes(discovered: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for preset in PRESET_ORDER {
        if preset == "procedural" || discovered.iter().any(|t| t == preset) {
            out.push(preset.to_string());
        }
    }
    let mut extras: Vec<String> = discovered
        .iter()
        .filter(|t| !out.iter().any(|existing| existing == *t))
        .cloned()
        .collect();
    extras.sort();
    extras.dedup();
    out.extend(extras);
    out
}

/// Persist the `[sounds]` section into `config.toml` atomically without clobbering other keys.
pub fn persist_sounds_config(path: &Path, sounds: &SoundsConfig) -> anyhow::Result<()> {
    let mut table: toml::Table = match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml::Table>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e.into()),
    };
    table.insert("sounds".into(), toml::Value::try_from(sounds)?);
    let serialized = toml::to_string_pretty(&table)?;
    paths::atomic_write(path, serialized.as_bytes())?;
    Ok(())
}

/// Complete desktop UI state (kept independent of OS window handles so all transitions are
/// headlessly unit-testable with `DISPLAY="" WAYLAND_DISPLAY=""`).
pub struct DesktopState {
    pub demo: bool,
    pub mix: ThemeMix,
    pub show: ShowConfig,
    pub sidebar_pref: bool,
    pub sound: bool,
    pub notifications: bool,
    pub sounds: SoundsConfig,
    pub ceo: CeoConfig,
    pub available_themes: Vec<String>,
    pub available_outfits: Vec<String>,
    pub selected_agent: Option<AgentId>,
    pub hovered_id: Option<AgentId>,
    pub active_tab: DockTab,

    // Whiteboard state
    pub whiteboard: Whiteboard,
    pub whiteboard_path: Option<PathBuf>,
    pub wb_new_title: String,

    // CEO Outfit editor draft fields
    pub outfit_draft_name: Option<String>,
    pub outfit_draft_full: bool,
    pub outfit_draft_primary: String,
    pub outfit_draft_secondary: String,
    pub outfit_error: Option<String>,

    // Inbox / Reports & Alerts
    pub inbox_only_boss: bool,
    pub selected_report_id: Option<u64>,
    pub alerts_cleared_at: Option<u64>,

    // Drag-and-drop / File picker confirmation modal
    pub import_modal: Option<ImportModalState>,

    // Config persistence path (None in unit tests that do not want disk writes)
    pub config_path: Option<PathBuf>,

    // Dismissible banners & auto-expiring toasts
    pub banners: Vec<Banner>,
    next_banner_id: u64,
    pub toasts: Vec<Toast>,
}

impl DesktopState {
    pub fn new(
        cfg: &Config,
        demo: bool,
        discovered_themes: Vec<String>,
        discovered_outfits: Vec<String>,
        whiteboard: Whiteboard,
        whiteboard_path: Option<PathBuf>,
        config_path: Option<PathBuf>,
    ) -> Self {
        let outfit_draft_name = cfg.ceo.outfit.clone();
        let outfit_draft_full = cfg.ceo.outfit_mode.eq_ignore_ascii_case("full");
        let outfit_draft_primary = cfg.ceo.primary_color.clone().unwrap_or_default();
        let outfit_draft_secondary = cfg.ceo.secondary_color.clone().unwrap_or_default();

        Self {
            demo,
            mix: cfg.theme.clone(),
            show: cfg.show.clone(),
            sidebar_pref: cfg.sidebar,
            sound: cfg.sound,
            notifications: cfg.notifications,
            sounds: cfg.sounds.clone(),
            ceo: cfg.ceo.clone(),
            available_themes: ordered_themes(&discovered_themes),
            available_outfits: discovered_outfits,
            selected_agent: None,
            hovered_id: None,
            active_tab: DockTab::Agents,
            whiteboard,
            whiteboard_path,
            wb_new_title: String::new(),
            outfit_draft_name,
            outfit_draft_full,
            outfit_draft_primary,
            outfit_draft_secondary,
            outfit_error: None,
            inbox_only_boss: true,
            selected_report_id: None,
            alerts_cleared_at: None,
            import_modal: None,
            config_path,
            banners: Vec::new(),
            next_banner_id: 1,
            toasts: Vec::new(),
        }
    }

    pub fn push_banner(&mut self, level: AlertLevel, message: impl Into<String>) {
        let msg = message.into();
        if self.banners.iter().any(|b| b.message == msg) {
            return;
        }
        let id = self.next_banner_id;
        self.next_banner_id += 1;
        self.banners.push(Banner { id, level, message: msg });
        if self.banners.len() > 4 {
            self.banners.remove(0);
        }
    }

    pub fn dismiss_banner(&mut self, id: u64) {
        self.banners.retain(|b| b.id != id);
    }

    pub fn push_toast(&mut self, level: AlertLevel, message: impl Into<String>, now_ms: u64) {
        self.toasts.push(Toast {
            level,
            message: message.into(),
            until_ms: now_ms.saturating_add(TOAST_DURATION_MS),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub fn expire_toasts(&mut self, now_ms: u64) {
        self.toasts.retain(|t| t.until_ms > now_ms);
    }

    /// Current uniform theme preset name (or `"custom"` when layers differ).
    pub fn active_theme_label(&self) -> &str {
        let f = &self.mix.floor;
        if self.mix.walls == *f
            && self.mix.furniture == *f
            && self.mix.decor == *f
            && self.mix.characters == *f
        {
            f.as_str()
        } else {
            "custom"
        }
    }

    /// Apply a single theme preset across all layers and persist config.
    pub fn select_theme_preset(&mut self, name: &str, now_ms: u64) -> Vec<UiAction> {
        self.mix = ThemeMix {
            floor: name.to_string(),
            walls: name.to_string(),
            furniture: name.to_string(),
            decor: name.to_string(),
            characters: name.to_string(),
        };
        self.persist_now(now_ms);
        self.push_toast(AlertLevel::Info, format!("Theme: {name}"), now_ms);
        vec![UiAction::ApplyThemeMix]
    }

    /// Cycle to the next available theme preset.
    pub fn cycle_theme_preset(&mut self, now_ms: u64) -> Vec<UiAction> {
        if self.available_themes.is_empty() {
            return Vec::new();
        }
        let cur = self.active_theme_label();
        let next_idx = self
            .available_themes
            .iter()
            .position(|t| t == cur)
            .map(|i| (i + 1) % self.available_themes.len())
            .unwrap_or(0);
        let next = self.available_themes[next_idx].clone();
        self.select_theme_preset(&next, now_ms)
    }

    /// Handle a click hit-test result on the pixel-art office viewport.
    pub fn handle_viewport_hit(&mut self, hit: Option<AgentId>) {
        match hit {
            Some(id) if id == WHITEBOARD_ID => {
                self.active_tab = DockTab::Whiteboard;
            }
            Some(id) if id == CEO_ID => {
                self.active_tab = DockTab::CeoOutfit;
            }
            Some(id) if id == CAT_ID => {
                self.selected_agent = None;
            }
            Some(id) => {
                self.selected_agent = Some(id);
                self.active_tab = DockTab::Agents;
            }
            None => {
                self.selected_agent = None;
            }
        }
    }

    /// Add a task from `wb_new_title` to the whiteboard, saving atomically.
    pub fn wb_add(&mut self, now_ms: u64) -> bool {
        let title = std::mem::take(&mut self.wb_new_title);
        if self.whiteboard.add(&title) {
            self.save_whiteboard(now_ms);
            true
        } else {
            false
        }
    }

    /// Toggle whiteboard item completion at `idx` and persist.
    pub fn wb_toggle(&mut self, idx: usize, now_ms: u64) -> bool {
        if self.whiteboard.toggle(idx) {
            self.save_whiteboard(now_ms);
            true
        } else {
            false
        }
    }

    /// Remove whiteboard item at `idx` and persist.
    pub fn wb_remove(&mut self, idx: usize, now_ms: u64) -> bool {
        if self.whiteboard.remove(idx) {
            self.save_whiteboard(now_ms);
            true
        } else {
            false
        }
    }

    /// Clear completed manual whiteboard items and persist.
    pub fn wb_clear_done(&mut self, now_ms: u64) -> usize {
        let n = self.whiteboard.clear_done();
        if n > 0 {
            self.save_whiteboard(now_ms);
        }
        n
    }

    /// Set wave-sync flag and persist.
    pub fn wb_set_sync_waves(&mut self, enabled: bool, scene: &Scene, now_ms: u64) -> bool {
        if self.whiteboard.sync_waves == enabled {
            return false;
        }
        self.whiteboard.sync_waves = enabled;
        if enabled {
            let _ = self.whiteboard.sync_from_scene(scene);
        }
        self.save_whiteboard(now_ms);
        true
    }

    fn save_whiteboard(&mut self, now_ms: u64) {
        if let Some(ref path) = self.whiteboard_path {
            if let Err(e) = self.whiteboard.save_to(path) {
                self.push_banner(AlertLevel::Warn, format!("Failed to save whiteboard: {e}"));
                self.push_toast(AlertLevel::Warn, "Whiteboard save failed", now_ms);
            }
        }
    }

    /// Validate and apply the CEO outfit draft to `self.ceo`, persisting config atomically.
    pub fn apply_outfit_draft(&mut self, now_ms: u64) -> Result<UiAction, String> {
        let primary = validate_optional_hex(&self.outfit_draft_primary)?;
        let secondary = validate_optional_hex(&self.outfit_draft_secondary)?;
        self.outfit_error = None;
        self.ceo.outfit = self.outfit_draft_name.clone();
        self.ceo.outfit_mode = if self.outfit_draft_full {
            "full".into()
        } else {
            "torso".into()
        };
        self.ceo.primary_color = primary;
        self.ceo.secondary_color = secondary;
        self.persist_now(now_ms);
        let label = self.ceo.outfit.as_deref().unwrap_or("procedural");
        self.push_toast(
            AlertLevel::Info,
            format!("CEO outfit applied: {label} ({})", self.ceo.outfit_mode),
            now_ms,
        );
        Ok(UiAction::ApplyCeo)
    }

    /// Open the import confirmation modal for a path picked via `rfd` or drag-and-drop.
    pub fn open_import_modal(&mut self, path: PathBuf, forced_kind: Option<ImportKind>) -> Result<(), String> {
        let suggested = suggest_pack_name(&path);
        let (kind, allowed) = match forced_kind {
            Some(k) => (k, vec![k]),
            None => match classify_dropped_path(&path) {
                Some(cls) => (cls.default_kind, cls.allowed_kinds),
                None => {
                    return Err(format!(
                        "Unsupported file or symlink: {} (drop a .png, .wav, or theme/outfit folder)",
                        path.display()
                    ));
                }
            },
        };
        let default_theme = self
            .available_themes
            .iter()
            .find(|t| *t != "procedural")
            .cloned()
            .unwrap_or_else(|| "custom".into());
        let pack_name = if kind == ImportKind::SpriteOverride {
            default_theme
        } else {
            suggested
        };
        self.import_modal = Some(ImportModalState {
            source_path: path,
            kind,
            allowed_kinds: allowed,
            pack_name,
            outfit_mode_full: false,
            sprite_name: SPRITE_NAMES[0].to_string(),
            sound_slot: SOUND_SLOTS[0].to_string(),
            confirm_overwrite: false,
            error: None,
        });
        Ok(())
    }

    pub fn persist_now(&mut self, now_ms: u64) {
        if let Some(ref path) = self.config_path {
            if let Err(e) = persist_config(
                path,
                &self.mix,
                &self.show,
                self.sidebar_pref,
                self.notifications,
                self.sound,
                &self.ceo,
            ) {
                self.push_banner(AlertLevel::Warn, format!("Failed to save config: {e}"));
                self.push_toast(AlertLevel::Warn, "Config save failed", now_ms);
            }
        }
    }

    pub fn visible_alerts<'a>(&self, scene: &'a Scene) -> Vec<&'a office_core::model::Alert> {
        let cutoff = self.alerts_cleared_at.unwrap_or(0);
        scene.alerts.iter().rev().filter(|a| a.at_ms > cutoff).collect()
    }

    pub fn clear_alerts(&mut self, now_ms: u64) {
        self.alerts_cleared_at = Some(now_ms);
    }
}

/// Apply our dark-studio `egui` visual theme once at startup.
pub fn configure_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(20, 22, 28);
    visuals.panel_fill = Color32::from_rgb(18, 20, 26);
    visuals.faint_bg_color = Color32::from_rgb(26, 29, 38);
    visuals.extreme_bg_color = Color32::from_rgb(13, 15, 20);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(44, 48, 60));
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(32, 36, 46);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(44, 50, 64);
    visuals.widgets.active.bg_fill = Color32::from_rgb(56, 64, 84);
    visuals.selection.bg_fill = Color32::from_rgb(56, 98, 168);
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::from_rgb(130, 180, 255));
    visuals.window_rounding = Rounding::same(8.0);
    ctx.set_visuals(visuals);
}

fn state_badge_color(state: AgentState) -> Color32 {
    match state {
        AgentState::Arriving | AgentState::Leaving => Color32::from_rgb(120, 126, 138),
        AgentState::Idle => Color32::from_rgb(142, 148, 162),
        AgentState::Thinking => Color32::from_rgb(186, 120, 240),
        AgentState::Typing => Color32::from_rgb(80, 210, 130),
        AgentState::Reading => Color32::from_rgb(82, 198, 224),
        AgentState::Running => Color32::from_rgb(98, 162, 250),
        AgentState::Delegating => Color32::from_rgb(218, 136, 245),
        AgentState::NeedsYou => Color32::from_rgb(245, 110, 96),
        AgentState::Queued => Color32::from_rgb(240, 196, 76),
        AgentState::Done => Color32::from_rgb(120, 228, 150),
        AgentState::Error => Color32::from_rgb(235, 78, 78),
    }
}

fn kind_badge_color(kind: AgentKind) -> Color32 {
    match kind {
        AgentKind::Claude => Color32::from_rgb(0xE8, 0x77, 0x3A),
        AgentKind::ClaudeSub => Color32::from_rgb(0xF2, 0xB1, 0x34),
        AgentKind::Agy => Color32::from_rgb(0x2E, 0xC4, 0xB6),
        AgentKind::Other => Color32::from_rgb(0x9A, 0xA0, 0xA6),
    }
}

fn alert_color(level: AlertLevel) -> Color32 {
    match level {
        AlertLevel::Info => Color32::from_rgb(140, 180, 235),
        AlertLevel::Warn => Color32::from_rgb(242, 196, 78),
        AlertLevel::Critical => Color32::from_rgb(240, 96, 96),
    }
}

/// Information passed from the renderer to the central viewport draw call.
pub struct ViewportFrame<'a> {
    pub texture: &'a egui::TextureHandle,
    pub logical_size: (u32, u32),
    pub backdrop: [u8; 4],
}

/// Output from drawing the viewport: the available size for next frame's layout, plus pointer events.
#[derive(Debug, Default)]
pub struct ViewportOutput {
    pub avail_points: (f32, f32),
    pub hover_logical: Option<(u32, u32)>,
    pub click_logical: Option<(u32, u32)>,
}

/// Draw the full desktop UI and return any requested `UiAction`s + viewport pointer telemetry.
pub fn draw_desktop_ui(
    ctx: &egui::Context,
    state: &mut DesktopState,
    scene: &Scene,
    frame: Option<ViewportFrame<'_>>,
    now_ms: u64,
) -> (Vec<UiAction>, ViewportOutput) {
    let mut actions = Vec::new();
    let mut vp_out = ViewportOutput::default();

    // 1. Handle OS drag-and-drop files (SEC-7: opens confirmation modal; never silently overwrites).
    let dropped = ctx.input(|i| i.raw.dropped_files.clone());
    for file in dropped {
        if let Some(path) = file.path {
            if let Err(err) = state.open_import_modal(path, None) {
                state.push_banner(AlertLevel::Warn, err);
            }
            break;
        }
    }

    // 2. Top toolbar.
    egui::TopBottomPanel::top("office_toolbar")
        .exact_height(38.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("AGENT OFFICE").strong().size(14.0).color(Color32::from_rgb(242, 238, 228)));
                if state.demo {
                    ui.label(
                        RichText::new("DEMO")
                            .strong()
                            .size(11.0)
                            .color(Color32::from_rgb(24, 24, 28))
                            .background_color(Color32::from_rgb(242, 196, 78)),
                    );
                }
                ui.separator();

                // Theme preset dropdown + cycle + reload.
                ui.label(RichText::new("Theme:").color(Color32::from_rgb(175, 182, 196)));
                let current_label = state.active_theme_label().to_string();
                let mut picked_theme: Option<String> = None;
                egui::ComboBox::from_id_salt("theme_preset_combo")
                    .selected_text(&current_label)
                    .width(126.0)
                    .show_ui(ui, |ui| {
                        for name in &state.available_themes {
                            if ui.selectable_label(current_label == *name, name).clicked() {
                                picked_theme = Some(name.clone());
                            }
                        }
                    });
                if let Some(name) = picked_theme {
                    actions.extend(state.select_theme_preset(&name, now_ms));
                }
                if ui.button("Next").on_hover_text("Cycle to next theme preset").clicked() {
                    actions.extend(state.cycle_theme_preset(now_ms));
                }
                if ui.button("Reload Themes").on_hover_text("Reload theme packs and sprites from disk").clicked() {
                    actions.push(UiAction::ReloadThemes);
                }

                ui.separator();

                // Sound toggle.
                let sound_lbl = if state.sound { "Sound: ON" } else { "Sound: OFF" };
                if ui.selectable_label(state.sound, sound_lbl).clicked() {
                    let next = !state.sound;
                    state.sound = next;
                    state.persist_now(now_ms);
                    actions.push(UiAction::ToggleSound(next));
                }

                // Notifications toggle.
                let notif_lbl = if state.notifications { "Notify: ON" } else { "Notify: OFF" };
                if ui.selectable_label(state.notifications, notif_lbl).clicked() {
                    let next = !state.notifications;
                    state.notifications = next;
                    state.persist_now(now_ms);
                    actions.push(UiAction::ToggleNotifications(next));
                }

                ui.separator();

                // Native file picker import menu (rfd).
                ui.menu_button("Import…", |ui| {
                    if ui.button("Theme Pack (folder)…").clicked() {
                        ui.close_menu();
                        if let Some(dir) = rfd::FileDialog::new().set_title("Import Theme Pack Folder").pick_folder() {
                            if let Err(e) = state.open_import_modal(dir, Some(ImportKind::ThemePack)) {
                                state.push_banner(AlertLevel::Warn, e);
                            }
                        }
                    }
                    if ui.button("CEO Outfit (PNG)…").clicked() {
                        ui.close_menu();
                        if let Some(file) = rfd::FileDialog::new()
                            .set_title("Import CEO Outfit PNG")
                            .add_filter("PNG Image", &["png"])
                            .pick_file()
                        {
                            if let Err(e) = state.open_import_modal(file, Some(ImportKind::CeoOutfit)) {
                                state.push_banner(AlertLevel::Warn, e);
                            }
                        }
                    }
                    if ui.button("CEO Outfit (folder)…").clicked() {
                        ui.close_menu();
                        if let Some(dir) = rfd::FileDialog::new().set_title("Import CEO Outfit Folder").pick_folder() {
                            if let Err(e) = state.open_import_modal(dir, Some(ImportKind::CeoOutfit)) {
                                state.push_banner(AlertLevel::Warn, e);
                            }
                        }
                    }
                    if ui.button("Sprite Override (PNG)…").clicked() {
                        ui.close_menu();
                        if let Some(file) = rfd::FileDialog::new()
                            .set_title("Import Sprite Override PNG")
                            .add_filter("PNG Image", &["png"])
                            .pick_file()
                        {
                            if let Err(e) = state.open_import_modal(file, Some(ImportKind::SpriteOverride)) {
                                state.push_banner(AlertLevel::Warn, e);
                            }
                        }
                    }
                    if ui.button("Sound WAV…").clicked() {
                        ui.close_menu();
                        if let Some(file) = rfd::FileDialog::new()
                            .set_title("Import Custom Sound WAV")
                            .add_filter("WAV Audio", &["wav"])
                            .pick_file()
                        {
                            if let Err(e) = state.open_import_modal(file, Some(ImportKind::SoundWav)) {
                                state.push_banner(AlertLevel::Warn, e);
                            }
                        }
                    }
                });

                // Right-aligned live summary stats.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cost = fmt::fmt_cost(scene.total_cost_usd);
                    let tokens = fmt::fmt_tokens(scene.total_tokens);
                    ui.label(
                        RichText::new(format!("{} agents · {} tok · {}", scene.agents.len(), tokens, cost))
                            .monospace()
                            .size(12.0)
                            .color(Color32::from_rgb(190, 198, 214)),
                    );
                });
            });
        });

    // 3. Dismissible non-fatal error/warning banners.
    if !state.banners.is_empty() {
        let mut to_dismiss = Vec::new();
        egui::TopBottomPanel::top("office_banners").show(ctx, |ui| {
            for b in &state.banners {
                let bg = match b.level {
                    AlertLevel::Critical => Color32::from_rgb(78, 26, 28),
                    AlertLevel::Warn => Color32::from_rgb(74, 58, 22),
                    AlertLevel::Info => Color32::from_rgb(28, 48, 76),
                };
                egui::Frame::none()
                    .fill(bg)
                    .stroke(Stroke::new(1.0_f32, alert_color(b.level)))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(fmt::level_tag(b.level)).strong().color(alert_color(b.level)));
                            ui.label(RichText::new(&b.message).color(Color32::from_rgb(242, 240, 236)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("Dismiss").clicked() {
                                    to_dismiss.push(b.id);
                                }
                            });
                        });
                    });
            }
        });
        for id in to_dismiss {
            state.dismiss_banner(id);
        }
    }

    // 4. Bottom toast / status bar.
    if !state.toasts.is_empty() {
        egui::TopBottomPanel::bottom("office_toasts")
            .exact_height(26.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some(t) = state.toasts.last() {
                        ui.label(RichText::new("●").color(alert_color(t.level)));
                        ui.label(RichText::new(&t.message).size(12.0).color(Color32::from_rgb(224, 228, 236)));
                    }
                });
            });
    }

    // 5. Right docked side panel with 4 tabs.
    egui::SidePanel::right("office_dock")
        .resizable(true)
        .default_width(350.0)
        .min_width(270.0)
        .max_width(540.0)
        .show(ctx, |ui| {
            let boss_count = scene.reports.iter().filter(|r| r.to.is_none()).count();
            let alert_count = state.visible_alerts(scene).len();
            let (wb_done, wb_total) = state.whiteboard.counts();

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut state.active_tab, DockTab::Agents, format!("Agents ({})", scene.agents.len()));
                let wb_lbl = if wb_total > 0 {
                    format!("Whiteboard ({wb_done}/{wb_total})")
                } else {
                    "Whiteboard".into()
                };
                ui.selectable_value(&mut state.active_tab, DockTab::Whiteboard, wb_lbl);
                ui.selectable_value(&mut state.active_tab, DockTab::CeoOutfit, "CEO Outfit");
                let inbox_lbl = if alert_count > 0 {
                    format!("Inbox ({boss_count}) · !{alert_count}")
                } else {
                    format!("Inbox ({boss_count})")
                };
                ui.selectable_value(&mut state.active_tab, DockTab::Inbox, inbox_lbl);
            });
            ui.separator();

            match state.active_tab {
                DockTab::Agents => draw_agents_tab(ui, state, scene, now_ms, &mut actions),
                DockTab::Whiteboard => draw_whiteboard_tab(ui, state, scene, now_ms, &mut actions),
                DockTab::CeoOutfit => draw_ceo_outfit_tab(ui, state, now_ms, &mut actions),
                DockTab::Inbox => draw_inbox_alerts_tab(ui, state, scene, now_ms),
            }
        });

    // 6. Central pixel-art office viewport.
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(Color32::from_rgb(14, 16, 22)))
        .show(ctx, |ui| {
            let avail = ui.available_rect_before_wrap();
            vp_out.avail_points = (avail.width().max(1.0), avail.height().max(1.0));

            if let Some(vf) = frame {
                let bg = Color32::from_rgba_unmultiplied(vf.backdrop[0], vf.backdrop[1], vf.backdrop[2], 255);
                ui.painter().rect_filled(avail, 0.0, bg);

                let (dx, dy, dw, dh) = aspect_fit_rect(
                    (avail.min.x, avail.min.y, avail.width(), avail.height()),
                    vf.logical_size,
                );
                let img_rect = Rect::from_min_size(Pos2::new(dx, dy), Vec2::new(dw, dh));
                ui.painter().image(
                    vf.texture.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );

                // Allocate interactive sensor across the viewport so clicks and hovers register cleanly.
                let resp = ui.allocate_rect(avail, egui::Sense::click());
                let raw_rect = (img_rect.min.x, img_rect.min.y, img_rect.width(), img_rect.height());
                if let Some(pos) = resp.hover_pos() {
                    vp_out.hover_logical = pointer_to_logical((pos.x, pos.y), raw_rect, vf.logical_size);
                }
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        vp_out.click_logical = pointer_to_logical((pos.x, pos.y), raw_rect, vf.logical_size);
                    }
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Initializing pixel-art office…").color(Color32::from_rgb(160, 168, 184)));
                });
            }

            // Visual drop-zone overlay while dragging files over the window.
            let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
            if hovering_files {
                ui.painter()
                    .rect_filled(avail, 0.0, Color32::from_rgba_unmultiplied(18, 28, 48, 185));
                ui.painter().rect_stroke(
                    avail.shrink(12.0),
                    Rounding::same(10.0),
                    Stroke::new(2.0_f32, Color32::from_rgb(110, 185, 255)),
                );
                ui.painter().text(
                    avail.center(),
                    egui::Align2::CENTER_CENTER,
                    "Drop Theme Pack folder, CEO Outfit PNG/folder, or Sound WAV to import",
                    egui::FontId::proportional(16.0),
                    Color32::WHITE,
                );
            }
        });

    // 7. Drag-and-drop / File picker Import Confirmation Modal (SEC-7).
    draw_import_modal(ctx, state, now_ms, &mut actions);

    (actions, vp_out)
}

fn draw_agents_tab(
    ui: &mut egui::Ui,
    state: &mut DesktopState,
    scene: &Scene,
    now_ms: u64,
    actions: &mut Vec<UiAction>,
) {
    // Router capacity & RAM bar.
    let r = &scene.router;
    egui::Frame::none()
        .fill(Color32::from_rgb(24, 27, 35))
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(44, 48, 60)))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Router").strong().size(12.0));
                if let Some(free) = r.free_mb {
                    ui.label(RichText::new(format!("{free} MB free")).monospace().size(11.0));
                }
                if let Some(p) = r.pressure {
                    let (plbl, pcol) = match p {
                        v if v >= 4 => ("CRIT", Color32::from_rgb(240, 96, 96)),
                        v if v >= 2 => ("WARN", Color32::from_rgb(242, 196, 78)),
                        _ => ("OK", Color32::from_rgb(96, 212, 132)),
                    };
                    ui.label(RichText::new(format!("RAM {plbl}")).strong().size(11.0).color(pcol));
                }
                if r.queued > 0 {
                    ui.label(
                        RichText::new(format!("queued: {}", r.queued))
                            .size(11.0)
                            .color(Color32::from_rgb(240, 196, 76)),
                    );
                }
            });
            let max_slots = r.max_concurrent.or(r.capacity).unwrap_or(0);
            if max_slots > 0 {
                let frac = (r.busy_slots as f32 / max_slots as f32).clamp(0.0, 1.0);
                let cap_note = r
                    .capacity
                    .map(|c| format!(" (cap {c})"))
                    .unwrap_or_default();
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!("Slots {}/{max_slots}{cap_note}", r.busy_slots))
                        .desired_height(15.0),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let idle_count = idle_kill_candidates(scene, now_ms).len();
                let kill_btn = ui.add_enabled(idle_count > 0, egui::Button::new(format!("Kill Idle ({idle_count})")));
                if kill_btn.clicked() {
                    actions.push(UiAction::KillIdleAgents);
                }
                if scene.router_extra.orphans > 0
                    && ui
                        .button(format!("Reap Orphans ({})", scene.router_extra.orphans))
                        .clicked()
                {
                    actions.push(UiAction::ReapOrphans);
                }
            });
        });

    ui.add_space(6.0);

    // Selected agent quick action strip.
    if let Some(sel_id) = state.selected_agent.clone() {
        if let Some(agent) = scene.agents.get(&sel_id) {
            egui::Frame::none()
                .fill(Color32::from_rgb(28, 34, 48))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(76, 114, 180)))
                .rounding(Rounding::same(6.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&agent.label).strong().color(kind_badge_color(agent.kind)));
                        ui.label(RichText::new(fmt::state_name(agent.state)).color(state_badge_color(agent.state)));
                    });
                    if !agent.activity.is_empty() {
                        ui.label(RichText::new(&agent.activity).size(11.5).color(Color32::from_rgb(205, 212, 224)));
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if let Some(ref req_id) = agent.pending_permission {
                            if ui
                                .button(RichText::new("Approve").color(Color32::from_rgb(110, 232, 140)))
                                .clicked()
                            {
                                actions.push(UiAction::ApprovePermission {
                                    id: agent.id.clone(),
                                    request_id: req_id.clone(),
                                });
                            }
                            if ui
                                .button(RichText::new("Deny").color(Color32::from_rgb(245, 110, 100)))
                                .clicked()
                            {
                                actions.push(UiAction::DenyPermission {
                                    id: agent.id.clone(),
                                    request_id: req_id.clone(),
                                });
                            }
                        }
                        if agent.resume_id.is_some() && ui.button("Resume").clicked() {
                            actions.push(UiAction::ResumeAgent { id: agent.id.clone() });
                        }
                        if let Some(pid) = agent.pid.filter(|&p| p > 1) {
                            if ui.button("Cancel").clicked() {
                                actions.push(UiAction::CancelAgent { id: agent.id.clone(), pid });
                            }
                        }
                    });
                });
            ui.add_space(6.0);
        }
    }

    // Live agent list.
    let rows = fmt::agent_rows(scene);
    if rows.is_empty() {
        ui.label(
            RichText::new("No active agents yet. Run Claude Code / Antigravity or start with --demo.")
                .italics()
                .color(Color32::from_rgb(142, 148, 162)),
        );
        return;
    }

    egui::ScrollArea::vertical().id_salt("agents_scroll").show(ui, |ui| {
        for (id, depth) in rows {
            let Some(agent) = scene.agents.get(&id) else {
                continue;
            };
            let is_sel = state.selected_agent.as_deref() == Some(id.as_str());
            draw_agent_card(ui, agent, depth, is_sel, now_ms, &mut state.selected_agent, actions);
            ui.add_space(4.0);
        }
    });
}

fn draw_agent_card(
    ui: &mut egui::Ui,
    agent: &Agent,
    depth: usize,
    is_sel: bool,
    now_ms: u64,
    selected: &mut Option<AgentId>,
    actions: &mut Vec<UiAction>,
) {
    let fill = if is_sel {
        Color32::from_rgb(36, 46, 68)
    } else {
        Color32::from_rgb(24, 27, 35)
    };
    let stroke = if is_sel {
        Stroke::new(1.2_f32, Color32::from_rgb(112, 168, 255))
    } else if agent.state == AgentState::NeedsYou {
        Stroke::new(1.2_f32, Color32::from_rgb(240, 102, 92))
    } else {
        Stroke::new(1.0_f32, Color32::from_rgb(42, 46, 58))
    };

    let indent = (depth.min(4) as f32) * 10.0;
    ui.horizontal(|ui| {
        if indent > 0.0 {
            ui.add_space(indent);
        }
        let resp = egui::Frame::none()
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(6.0))
            .inner_margin(egui::Margin::same(7.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let badge_col = state_badge_color(agent.state);
                    ui.label(RichText::new("●").color(badge_col));
                    let name = fmt::tree_label(&agent.label, depth);
                    if ui
                        .selectable_label(is_sel, RichText::new(name).strong().color(kind_badge_color(agent.kind)))
                        .clicked()
                    {
                        *selected = Some(agent.id.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let elapsed = fmt::fmt_elapsed_short(now_ms.saturating_sub(agent.started_ms));
                        ui.label(RichText::new(elapsed).monospace().size(11.0).color(Color32::from_rgb(150, 156, 170)));
                        ui.label(
                            RichText::new(fmt::state_short(agent.state))
                                .strong()
                                .size(11.0)
                                .color(badge_col),
                        );
                    });
                });

                // Metrics row: cost, tokens, RSS, model/role.
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "${} · {} tok",
                            fmt::fmt_cost_short(agent.cost_usd),
                            fmt::fmt_tokens(agent.tokens_shown())
                        ))
                        .monospace()
                        .size(11.0)
                        .color(Color32::from_rgb(176, 184, 200)),
                    );
                    if let Some(rss) = agent.rss_mb {
                        ui.label(
                            RichText::new(format!("{rss} MB"))
                                .monospace()
                                .size(11.0)
                                .color(Color32::from_rgb(156, 166, 184)),
                        );
                    }
                    let rm = fmt::role_or_model(agent);
                    if rm != "-" {
                        ui.label(RichText::new(rm).size(11.0).color(Color32::from_rgb(138, 146, 164)));
                    }
                });

                if let Some(ctx_pct) = agent.context_pct {
                    let frac = (ctx_pct / 100.0).clamp(0.0, 1.0);
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .text(format!("Context {ctx_pct:.0}%"))
                            .desired_height(13.0),
                    );
                }

                if !agent.activity.is_empty() {
                    ui.label(
                        RichText::new(fmt::trunc(&agent.activity, 64))
                            .size(11.5)
                            .color(Color32::from_rgb(208, 214, 226)),
                    );
                }

                // Inline permission Approve / Deny buttons.
                if let Some(ref req_id) = agent.pending_permission {
                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        if ui
                            .small_button(RichText::new("✓ Approve").color(Color32::from_rgb(110, 232, 140)))
                            .clicked()
                        {
                            actions.push(UiAction::ApprovePermission {
                                id: agent.id.clone(),
                                request_id: req_id.clone(),
                            });
                        }
                        if ui
                            .small_button(RichText::new("✕ Deny").color(Color32::from_rgb(245, 110, 100)))
                            .clicked()
                        {
                            actions.push(UiAction::DenyPermission {
                                id: agent.id.clone(),
                                request_id: req_id.clone(),
                            });
                        }
                    });
                }
            })
            .response;
        if resp.clicked() {
            *selected = Some(agent.id.clone());
        }
    });
}

fn draw_whiteboard_tab(
    ui: &mut egui::Ui,
    state: &mut DesktopState,
    scene: &Scene,
    now_ms: u64,
    actions: &mut Vec<UiAction>,
) {
    let (done, total) = state.whiteboard.counts();
    let pct = state.whiteboard.completion_pct();
    let frac = if total == 0 {
        0.0
    } else {
        done as f32 / total as f32
    };

    ui.label(RichText::new("Office Whiteboard").strong().size(13.5));
    ui.add_space(4.0);
    ui.add(
        egui::ProgressBar::new(frac)
            .text(format!("{pct}% ({done}/{total})"))
            .desired_height(18.0),
    );
    ui.add_space(6.0);

    // Wave sync toggle + Clear Done button.
    let mut wb_changed = false;
    ui.horizontal(|ui| {
        let mut sync_waves = state.whiteboard.sync_waves;
        if ui.checkbox(&mut sync_waves, "Sync ROSSO waves").changed()
            && state.wb_set_sync_waves(sync_waves, scene, now_ms)
        {
            wb_changed = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let has_done_manual = state
                .whiteboard
                .items
                .iter()
                .any(|i| i.done && i.source == TaskSource::Manual);
            if ui
                .add_enabled(has_done_manual, egui::Button::new("Clear Done"))
                .clicked()
            {
                let removed = state.wb_clear_done(now_ms);
                if removed > 0 {
                    wb_changed = true;
                    state.push_toast(AlertLevel::Info, format!("Cleared {removed} done task(s)"), now_ms);
                }
            }
        });
    });

    ui.add_space(6.0);

    // Add-task input row.
    ui.horizontal(|ui| {
        let te = ui.add(
            egui::TextEdit::singleline(&mut state.wb_new_title)
                .hint_text("Add a whiteboard task…")
                .desired_width(ui.available_width() - 56.0),
        );
        let submit = (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.button("Add").clicked();
        if submit && state.wb_add(now_ms) {
            wb_changed = true;
        }
    });

    ui.separator();

    // Checklist items.
    let mut toggle_idx = None;
    let mut remove_idx = None;
    if state.whiteboard.items.is_empty() {
        ui.label(
            RichText::new("No tasks yet. Add one above to draw it on the back-wall whiteboard.")
                .italics()
                .color(Color32::from_rgb(142, 148, 162)),
        );
    } else {
        egui::ScrollArea::vertical().id_salt("whiteboard_scroll").show(ui, |ui| {
            for (idx, item) in state.whiteboard.items.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mut checked = item.done;
                    if ui.checkbox(&mut checked, "").changed() {
                        toggle_idx = Some(idx);
                    }
                    let text = if item.done {
                        RichText::new(&item.title).strikethrough().color(Color32::from_rgb(130, 138, 152))
                    } else {
                        RichText::new(&item.title).color(Color32::from_rgb(230, 234, 242))
                    };
                    ui.label(text);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Delete task").clicked() {
                            remove_idx = Some(idx);
                        }
                        if item.source == TaskSource::Wave {
                            ui.label(
                                RichText::new("WAVE")
                                    .size(10.0)
                                    .color(Color32::from_rgb(90, 200, 190)),
                            );
                        }
                    });
                });
            }
        });
    }

    if let Some(i) = toggle_idx {
        if state.wb_toggle(i, now_ms) {
            wb_changed = true;
        }
    }
    if let Some(i) = remove_idx {
        if state.wb_remove(i, now_ms) {
            wb_changed = true;
        }
    }

    if wb_changed {
        actions.push(UiAction::SyncWhiteboardToRenderer);
    }
}

fn draw_ceo_outfit_tab(
    ui: &mut egui::Ui,
    state: &mut DesktopState,
    now_ms: u64,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("CEO Outfit & Colors").strong().size(13.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Refresh").clicked() {
                actions.push(UiAction::RefreshOutfits);
            }
        });
    });
    ui.add_space(6.0);

    // Outfit selector.
    ui.label("Installed Outfit Pack:");
    let current_text = state
        .outfit_draft_name
        .as_deref()
        .unwrap_or("(procedural)")
        .to_string();
    egui::ComboBox::from_id_salt("ceo_outfit_combo")
        .selected_text(&current_text)
        .width(220.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(state.outfit_draft_name.is_none(), "(procedural)")
                .clicked()
            {
                state.outfit_draft_name = None;
            }
            for name in &state.available_outfits {
                let is_sel = state.outfit_draft_name.as_deref() == Some(name.as_str());
                if ui.selectable_label(is_sel, name).clicked() {
                    state.outfit_draft_name = Some(name.clone());
                }
            }
        });

    ui.add_space(8.0);

    // Outfit mode (Torso / Full).
    ui.label("Overlay Mode:");
    ui.horizontal(|ui| {
        ui.radio_value(&mut state.outfit_draft_full, false, "Torso (keep procedural head)");
        ui.radio_value(&mut state.outfit_draft_full, true, "Full sprite");
    });

    ui.add_space(8.0);

    // Primary / Secondary hex color overrides.
    ui.label("Optional Color Overrides (#rrggbb):");
    ui.horizontal(|ui| {
        ui.label("Primary:    ");
        ui.add(
            egui::TextEdit::singleline(&mut state.outfit_draft_primary)
                .hint_text("#d0202a")
                .desired_width(110.0),
        );
        if let Some(c) = parse_hex_color32(&state.outfit_draft_primary) {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, c);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Secondary:");
        ui.add(
            egui::TextEdit::singleline(&mut state.outfit_draft_secondary)
                .hint_text("#18181b")
                .desired_width(110.0),
        );
        if let Some(c) = parse_hex_color32(&state.outfit_draft_secondary) {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, c);
        }
    });

    if let Some(ref err) = state.outfit_error {
        ui.add_space(4.0);
        ui.label(RichText::new(err).color(Color32::from_rgb(245, 110, 100)).size(11.5));
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui.button("Apply & Save").clicked() {
            match state.apply_outfit_draft(now_ms) {
                Ok(act) => actions.push(act),
                Err(e) => state.outfit_error = Some(e),
            }
        }
        if ui.button("Reset Colors").clicked() {
            state.outfit_draft_primary.clear();
            state.outfit_draft_secondary.clear();
            state.outfit_error = None;
        }
    });

    ui.add_space(10.0);
    ui.label(
        RichText::new("Tip: Drag a PNG or outfit folder onto the window (or use Import…) to install new CEO outfits.")
            .size(11.5)
            .color(Color32::from_rgb(142, 148, 162)),
    );
}

fn draw_inbox_alerts_tab(ui: &mut egui::Ui, state: &mut DesktopState, scene: &Scene, now_ms: u64) {
    // Section 1: Boss Inbox / Reports
    ui.horizontal(|ui| {
        ui.label(RichText::new("Boss Inbox & Reports").strong().size(13.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.selectable_value(&mut state.inbox_only_boss, false, "All");
            ui.selectable_value(&mut state.inbox_only_boss, true, "Boss Only");
        });
    });

    let reports = report_list_for(scene, None, state.inbox_only_boss);
    if reports.is_empty() {
        ui.label(
            RichText::new("No reports in inbox yet.")
                .italics()
                .color(Color32::from_rgb(142, 148, 162)),
        );
    } else {
        // Default-select the newest report if none selected or selected no longer exists.
        if state
            .selected_report_id
            .is_none_or(|id| !reports.iter().any(|r| r.id == id))
        {
            state.selected_report_id = Some(reports[0].id);
        }

        egui::ScrollArea::vertical()
            .id_salt("inbox_list_scroll")
            .max_height(155.0)
            .show(ui, |ui| {
                for r in &reports {
                    let is_sel = state.selected_report_id == Some(r.id);
                    let kind_tag = match r.kind {
                        ReportKind::Result => "RESULT",
                        ReportKind::Report => "REPORT",
                        ReportKind::Task => "TASK",
                    };
                    let ago = fmt::fmt_ago(now_ms, r.at_ms);
                    let label = format!("[{kind_tag}] {} · {} ({ago})", r.from_label, fmt::trunc(&r.title, 30));
                    if ui.selectable_label(is_sel, label).clicked() {
                        state.selected_report_id = Some(r.id);
                    }
                }
            });

        ui.add_space(4.0);
        if let Some(sel_id) = state.selected_report_id {
            if let Some(report) = reports.iter().find(|r| r.id == sel_id) {
                egui::Frame::none()
                    .fill(Color32::from_rgb(22, 25, 33))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(44, 48, 60)))
                    .rounding(Rounding::same(6.0))
                    .inner_margin(egui::Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&report.title).strong().size(13.0));
                        ui.label(
                            RichText::new(format!("From {} → {}", report.from_label, report.to_label))
                                .size(11.0)
                                .color(Color32::from_rgb(150, 158, 174)),
                        );
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("inbox_preview_scroll")
                            .max_height(200.0)
                            .show(ui, |ui| {
                                render_markdown_preview(ui, &report.body);
                            });
                    });
            }
        }
    }

    ui.add_space(8.0);
    ui.separator();

    // Section 2: Active Alerts
    let alerts = state.visible_alerts(scene);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Alerts ({})", alerts.len())).strong().size(13.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(!alerts.is_empty(), egui::Button::new("Clear")).clicked() {
                state.clear_alerts(now_ms);
            }
        });
    });

    if alerts.is_empty() {
        ui.label(
            RichText::new("No active alerts.")
                .italics()
                .color(Color32::from_rgb(142, 148, 162)),
        );
    } else {
        egui::ScrollArea::vertical().id_salt("alerts_scroll").show(ui, |ui| {
            for a in alerts {
                let col = alert_color(a.level);
                let ago = fmt::fmt_ago(now_ms, a.at_ms);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(fmt::level_tag(a.level)).strong().size(11.0).color(col));
                    ui.label(RichText::new(format!("[{ago}]")).monospace().size(11.0).color(Color32::from_rgb(140, 148, 162)));
                    ui.label(RichText::new(&a.message).size(11.5));
                });
            }
        });
    }
}

/// Lightweight, clean Markdown preview renderer for Boss Inbox reports.
fn render_markdown_preview(ui: &mut egui::Ui, body: &str) {
    if body.trim().is_empty() {
        ui.label(RichText::new("(empty report body)").italics().color(Color32::from_rgb(140, 148, 162)));
        return;
    }
    let mut in_code_block = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            ui.label(
                RichText::new(line)
                    .monospace()
                    .size(11.5)
                    .color(Color32::from_rgb(172, 224, 198)),
            );
        } else if let Some(h) = trimmed.strip_prefix("### ") {
            ui.label(RichText::new(h).strong().size(12.5).color(Color32::from_rgb(218, 226, 242)));
        } else if let Some(h) = trimmed.strip_prefix("## ") {
            ui.label(RichText::new(h).strong().size(13.5).color(Color32::from_rgb(236, 240, 248)));
        } else if let Some(h) = trimmed.strip_prefix("# ") {
            ui.label(RichText::new(h).strong().size(14.5).color(Color32::WHITE));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("•").color(Color32::from_rgb(120, 175, 245)));
                ui.label(RichText::new(item).size(12.0));
            });
        } else if trimmed.is_empty() {
            ui.add_space(4.0);
        } else {
            ui.label(RichText::new(line).size(12.0).color(Color32::from_rgb(214, 218, 228)));
        }
    }
}

fn draw_import_modal(
    ctx: &egui::Context,
    state: &mut DesktopState,
    now_ms: u64,
    actions: &mut Vec<UiAction>,
) {
    let Some(mut modal) = state.import_modal.take() else {
        return;
    };
    let mut keep_open = true;
    let mut commit_import = false;

    egui::Window::new("Import Asset")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(430.0)
        .max_width(430.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(format!("Source: {}", modal.source_path.display()))
                    .monospace()
                    .size(11.5)
                    .color(Color32::from_rgb(180, 188, 204)),
            );
            ui.add_space(6.0);

            // Kind selector if multiple targets are allowed for this file/folder.
            if modal.allowed_kinds.len() > 1 {
                ui.horizontal(|ui| {
                    ui.label("Import as:");
                    for &k in &modal.allowed_kinds {
                        if ui.radio(modal.kind == k, k.label()).clicked() && modal.kind != k {
                            modal.kind = k;
                            modal.confirm_overwrite = false;
                            modal.error = None;
                        }
                    }
                });
            } else {
                ui.label(RichText::new(format!("Target: {}", modal.kind.label())).strong());
            }

            ui.add_space(6.0);

            let mut name_valid = true;
            match modal.kind {
                ImportKind::ThemePack | ImportKind::CeoOutfit | ImportKind::SpriteOverride => {
                    let lbl = match modal.kind {
                        ImportKind::ThemePack => "Theme pack name:",
                        ImportKind::CeoOutfit => "Outfit pack name:",
                        ImportKind::SpriteOverride => "Target theme name:",
                        ImportKind::SoundWav => unreachable!(),
                    };
                    ui.horizontal(|ui| {
                        ui.label(lbl);
                        if ui.text_edit_singleline(&mut modal.pack_name).changed() {
                            modal.confirm_overwrite = false;
                            modal.error = None;
                        }
                    });
                    if let Err(err) = sanitize_pack_name(&modal.pack_name) {
                        name_valid = false;
                        ui.label(RichText::new(err).size(11.0).color(Color32::from_rgb(245, 110, 100)));
                    }
                }
                ImportKind::SoundWav => {}
            }

            if modal.kind == ImportKind::CeoOutfit {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    ui.radio_value(&mut modal.outfit_mode_full, false, "Torso");
                    ui.radio_value(&mut modal.outfit_mode_full, true, "Full");
                });
            }

            if modal.kind == ImportKind::SpriteOverride {
                ui.horizontal(|ui| {
                    ui.label("Sprite slot:");
                    egui::ComboBox::from_id_salt("import_sprite_slot")
                        .selected_text(&modal.sprite_name)
                        .show_ui(ui, |ui| {
                            for &sp in &SPRITE_NAMES {
                                if ui.selectable_label(modal.sprite_name == sp, sp).clicked() {
                                    modal.sprite_name = sp.to_string();
                                    modal.confirm_overwrite = false;
                                }
                            }
                        });
                });
            }

            if modal.kind == ImportKind::SoundWav {
                ui.horizontal(|ui| {
                    ui.label("Sound slot:");
                    egui::ComboBox::from_id_salt("import_sound_slot")
                        .selected_text(&modal.sound_slot)
                        .show_ui(ui, |ui| {
                            for &slot in &SOUND_SLOTS {
                                if ui.selectable_label(modal.sound_slot == slot, slot).clicked() {
                                    modal.sound_slot = slot.to_string();
                                    modal.confirm_overwrite = false;
                                }
                            }
                        });
                });
            }

            // SEC-7: explicit overwrite warning + mandatory confirmation checkbox.
            let overwrite_path = check_import_overwrite(
                modal.kind,
                &modal.pack_name,
                &modal.sprite_name,
                &modal.sound_slot,
                &paths::user_themes_dir(),
                &paths::outfits_dir(),
                &paths::sounds_dir(),
            );
            if let Some(ref existing) = overwrite_path {
                ui.add_space(6.0);
                egui::Frame::none()
                    .fill(Color32::from_rgb(76, 54, 20))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(242, 196, 78)))
                    .rounding(Rounding::same(4.0))
                    .inner_margin(egui::Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("Warning: {} already exists and will be overwritten.", existing.display()))
                                .color(Color32::from_rgb(252, 224, 132))
                                .size(11.5),
                        );
                        ui.checkbox(&mut modal.confirm_overwrite, "I confirm overwriting the existing asset");
                    });
            }

            if let Some(ref err) = modal.error {
                ui.add_space(6.0);
                ui.label(RichText::new(err).color(Color32::from_rgb(245, 110, 100)).size(11.5));
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let can_import = name_valid && (overwrite_path.is_none() || modal.confirm_overwrite);
                if ui.add_enabled(can_import, egui::Button::new("Import")).clicked() {
                    commit_import = true;
                }
                if ui.button("Cancel").clicked() {
                    keep_open = false;
                }
            });
        });

    if commit_import {
        let res: Result<String, String> = match modal.kind {
            ImportKind::ThemePack => import_theme_pack(&modal.source_path, &modal.pack_name).map(|p| {
                actions.push(UiAction::ReloadThemes);
                format!("Imported theme '{}' -> {}", modal.pack_name, p.display())
            }),
            ImportKind::CeoOutfit => {
                let mode = if modal.outfit_mode_full { "full" } else { "torso" };
                import_ceo_outfit(&modal.source_path, &modal.pack_name, Some(mode)).map(|p| {
                    state.available_outfits = list_outfits();
                    state.outfit_draft_name = Some(modal.pack_name.clone());
                    state.outfit_draft_full = modal.outfit_mode_full;
                    actions.push(UiAction::RefreshOutfits);
                    format!("Imported CEO outfit '{}' -> {}", modal.pack_name, p.display())
                })
            }
            ImportKind::SpriteOverride => {
                import_sprite_override(&modal.pack_name, &modal.sprite_name, &modal.source_path).map(|p| {
                    actions.push(UiAction::ReloadThemes);
                    format!(
                        "Imported sprite {}/{} -> {}",
                        modal.pack_name,
                        modal.sprite_name,
                        p.display()
                    )
                })
            }
            ImportKind::SoundWav => import_sound(&modal.sound_slot, &modal.source_path).map(|p| {
                let file_name = format!("{}.wav", modal.sound_slot);
                match modal.sound_slot.as_str() {
                    "test" => state.sounds.test = Some(file_name),
                    "needs-you" => state.sounds.needs_you = Some(file_name),
                    "inbox" => state.sounds.inbox = Some(file_name),
                    "error" => state.sounds.error = Some(file_name),
                    _ => {}
                }
                if let Some(ref cfg_path) = state.config_path {
                    if let Err(e) = persist_sounds_config(cfg_path, &state.sounds) {
                        state.push_banner(AlertLevel::Warn, format!("Imported WAV, but failed to persist config: {e}"));
                    }
                }
                actions.push(UiAction::SoundSlotUpdated(state.sounds.clone()));
                format!("Imported sound '{}' -> {}", modal.sound_slot, p.display())
            }),
        };

        match res {
            Ok(msg) => {
                state.push_toast(AlertLevel::Info, msg, now_ms);
                keep_open = false;
            }
            Err(err) => {
                modal.error = Some(err);
                keep_open = true;
            }
        }
    }

    if keep_open {
        state.import_modal = Some(modal);
    }
}

/// Convert `Chime` variant name for concise error reporting.
pub fn chime_label(chime: Chime) -> &'static str {
    match chime {
        Chime::Test => "test",
        Chime::NeedsYou => "needs-you",
        Chime::Inbox => "inbox",
        Chime::Error => "error",
    }
}

/// Helper to build a nearest-neighbor `ColorImage` from an `image::RgbaImage` buffer.
pub fn rgba_to_color_image(img: &image::RgbaImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

/// Texture options ensuring crisp nearest-neighbor pixel-art rendering.
pub fn pixel_texture_options() -> TextureOptions {
    TextureOptions::NEAREST
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "office-desktop-ui-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pointer_to_logical_maps_center_edges_outside_and_hidpi_aspect_fit() {
        let rect = (100.0, 50.0, 400.0, 200.0);
        let logical = (200, 100);

        // Center
        assert_eq!(pointer_to_logical((300.0, 150.0), rect, logical), Some((100, 50)));
        // Top-left exact edge
        assert_eq!(pointer_to_logical((100.0, 50.0), rect, logical), Some((0, 0)));
        // Bottom-right just inside
        assert_eq!(pointer_to_logical((499.9, 249.9), rect, logical), Some((199, 99)));
        // Exact max boundary is outside
        assert_eq!(pointer_to_logical((500.0, 150.0), rect, logical), None);
        assert_eq!(pointer_to_logical((300.0, 250.0), rect, logical), None);
        // Outside left/top
        assert_eq!(pointer_to_logical((99.9, 100.0), rect, logical), None);
        assert_eq!(pointer_to_logical((200.0, 49.9), rect, logical), None);
        // Degenerate inputs
        assert_eq!(pointer_to_logical((0.0, 0.0), (0.0, 0.0, 0.0, 100.0), logical), None);
        assert_eq!(pointer_to_logical((10.0, 10.0), rect, (0, 100)), None);

        // HiDPI aspect-fitted rect: e.g. wide 900x400 logical-point viewport containing a 320x200 office.
        let fitted = aspect_fit_rect((20.0, 40.0, 900.0, 400.0), (320, 200));
        // Height-limited: scale = 2.0 -> 640x400 centered horizontally at x = 20 + (900 - 640)/2 = 150
        assert!((fitted.0 - 150.0).abs() < 0.01);
        assert!((fitted.1 - 40.0).abs() < 0.01);
        assert!((fitted.2 - 640.0).abs() < 0.01);
        assert!((fitted.3 - 400.0).abs() < 0.01);
        // Letterbox margins return None
        assert_eq!(pointer_to_logical((149.0, 200.0), fitted, (320, 200)), None);
        assert_eq!(pointer_to_logical((791.0, 200.0), fitted, (320, 200)), None);
        // Center maps to (160, 100)
        assert_eq!(pointer_to_logical((470.0, 240.0), fitted, (320, 200)), Some((160, 100)));

        // Round-trip logical -> renderer hit_test coords -> back to logical
        for scale in [1.0f32, 2.0, 3.0, 4.0] {
            let (ox, oy) = (17u32, 9u32);
            let (fx, fy) = logical_to_hit_coords((160, 100), ox, oy, scale);
            let back_x = ((fx as f32 - ox as f32) / scale).floor() as u32;
            let back_y = ((fy as f32 - oy as f32) / scale).floor() as u32;
            assert_eq!((back_x, back_y), (160, 100));
        }
    }

    #[test]
    fn drag_and_drop_classification_and_overwrite_detection() {
        let dir = tmp_dir("dnd");
        let png = dir.join("My Cool Outfit.PNG");
        std::fs::write(&png, b"fake").unwrap();
        let wav = dir.join("Alert Chime.wav");
        std::fs::write(&wav, b"fake").unwrap();
        let txt = dir.join("readme.txt");
        std::fs::write(&txt, b"hello").unwrap();

        let theme_dir = dir.join("Neon Theme");
        std::fs::create_dir_all(&theme_dir).unwrap();
        std::fs::write(theme_dir.join("theme.toml"), "[colors]\n").unwrap();

        let outfit_dir = dir.join("Boss Tuxedo");
        std::fs::create_dir_all(&outfit_dir).unwrap();
        std::fs::write(outfit_dir.join("outfit.toml"), "sprite = \"outfit.png\"\n").unwrap();

        let cls_png = classify_dropped_path(&png).unwrap();
        assert_eq!(cls_png.default_kind, ImportKind::CeoOutfit);
        assert_eq!(cls_png.allowed_kinds, vec![ImportKind::CeoOutfit, ImportKind::SpriteOverride]);
        assert_eq!(cls_png.suggested_name, "my-cool-outfit");

        let cls_wav = classify_dropped_path(&wav).unwrap();
        assert_eq!(cls_wav.default_kind, ImportKind::SoundWav);
        assert_eq!(cls_wav.allowed_kinds, vec![ImportKind::SoundWav]);

        assert!(classify_dropped_path(&txt).is_none());

        let cls_theme = classify_dropped_path(&theme_dir).unwrap();
        assert_eq!(cls_theme.default_kind, ImportKind::ThemePack);
        assert_eq!(cls_theme.suggested_name, "neon-theme");

        let cls_outfit = classify_dropped_path(&outfit_dir).unwrap();
        assert_eq!(cls_outfit.default_kind, ImportKind::CeoOutfit);
        assert_eq!(cls_outfit.suggested_name, "boss-tuxedo");

        // Overwrite detection
        let themes_root = dir.join("installed_themes");
        let outfits_root = dir.join("installed_outfits");
        let sounds_root = dir.join("installed_sounds");
        std::fs::create_dir_all(themes_root.join("neon/sprites")).unwrap();
        std::fs::write(themes_root.join("neon/sprites/desk.png"), b"x").unwrap();
        std::fs::create_dir_all(outfits_root.join("tux")).unwrap();
        std::fs::create_dir_all(&sounds_root).unwrap();
        std::fs::write(sounds_root.join("inbox.wav"), b"x").unwrap();

        assert!(check_import_overwrite(
            ImportKind::ThemePack,
            "neon",
            "desk",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_some());
        assert!(check_import_overwrite(
            ImportKind::ThemePack,
            "fresh",
            "desk",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_none());
        assert!(check_import_overwrite(
            ImportKind::CeoOutfit,
            "tux",
            "desk",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_some());
        assert!(check_import_overwrite(
            ImportKind::SpriteOverride,
            "neon",
            "desk",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_some());
        assert!(check_import_overwrite(
            ImportKind::SpriteOverride,
            "neon",
            "chair",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_none());
        assert!(check_import_overwrite(
            ImportKind::SoundWav,
            "unused",
            "desk",
            "inbox",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_some());
        assert!(check_import_overwrite(
            ImportKind::SoundWav,
            "unused",
            "desk",
            "error",
            &themes_root,
            &outfits_root,
            &sounds_root
        )
        .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn desktop_state_whiteboard_tabs_and_outfit_transitions() {
        let dir = tmp_dir("state");
        let wb_path = dir.join("whiteboard.json");
        let cfg_path = dir.join("config.toml");
        let cfg = Config::default();

        let mut st = DesktopState::new(
            &cfg,
            true,
            vec!["procedural".into(), "cozy".into(), "zebra".into()],
            vec!["tuxedo".into()],
            Whiteboard::default(),
            Some(wb_path.clone()),
            Some(cfg_path.clone()),
        );

        // Theme ordering puts presets before custom packs.
        assert_eq!(st.available_themes, vec!["procedural", "cozy", "zebra"]);

        // Viewport clicks switch tabs & selection.
        assert_eq!(st.active_tab, DockTab::Agents);
        st.handle_viewport_hit(Some(WHITEBOARD_ID.into()));
        assert_eq!(st.active_tab, DockTab::Whiteboard);
        st.handle_viewport_hit(Some("cc:alpha".into()));
        assert_eq!(st.active_tab, DockTab::Agents);
        assert_eq!(st.selected_agent.as_deref(), Some("cc:alpha"));
        st.handle_viewport_hit(Some(CEO_ID.into()));
        assert_eq!(st.active_tab, DockTab::CeoOutfit);
        st.handle_viewport_hit(None);
        assert_eq!(st.selected_agent, None);

        // Whiteboard add, toggle, persist, clear_done.
        st.wb_new_title = "Ship desktop app".into();
        assert!(st.wb_add(1_000));
        st.wb_new_title = "Write headless tests".into();
        assert!(st.wb_add(1_001));
        assert_eq!(st.whiteboard.counts(), (0, 2));
        assert!(st.wb_toggle(0, 1_002));
        assert_eq!(st.whiteboard.completion_pct(), 50);

        let (reloaded, warn) = Whiteboard::load_from(&wb_path);
        assert!(warn.is_none());
        assert_eq!(reloaded.counts(), (1, 2));

        assert_eq!(st.wb_clear_done(1_003), 1);
        assert_eq!(st.whiteboard.counts(), (0, 1));

        // Outfit selection + hex validation + config persistence.
        st.outfit_draft_name = Some("tuxedo".into());
        st.outfit_draft_full = true;
        st.outfit_draft_primary = "not-a-hex".into();
        assert!(st.apply_outfit_draft(2_000).is_err());

        st.outfit_draft_primary = "#112233".into();
        st.outfit_draft_secondary = "#AABBCC".into();
        assert_eq!(st.apply_outfit_draft(2_001).unwrap(), UiAction::ApplyCeo);
        assert_eq!(st.ceo.outfit.as_deref(), Some("tuxedo"));
        assert_eq!(st.ceo.outfit_mode, "full");
        assert_eq!(st.ceo.primary_color.as_deref(), Some("#112233"));
        assert_eq!(st.ceo.secondary_color.as_deref(), Some("#aabbcc"));

        let saved_cfg = Config::load_from(&cfg_path);
        assert_eq!(saved_cfg.ceo.outfit.as_deref(), Some("tuxedo"));
        assert_eq!(saved_cfg.ceo.outfit_mode, "full");
        assert_eq!(saved_cfg.ceo.primary_color.as_deref(), Some("#112233"));

        // Banner add + dismiss.
        st.push_banner(AlertLevel::Warn, "test banner");
        assert_eq!(st.banners.len(), 1);
        let bid = st.banners[0].id;
        st.dismiss_banner(bid);
        assert!(st.banners.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn headless_egui_renders_all_tabs_and_import_modal() {
        let dir = tmp_dir("egui-headless");
        let sample_png = dir.join("outfit.png");
        std::fs::write(&sample_png, b"fake").unwrap();

        let cfg = Config::default();
        let scene = office::render::demo::demo_scene(50_000);
        let mut renderer = office::render::Renderer::new(&cfg);
        let mut wb = Whiteboard::default();
        wb.add("Inspect pixel-art canvas");
        wb.add("Verify docked tabs");
        wb.toggle(0);
        renderer.set_whiteboard(&wb);

        let mut st = DesktopState::new(
            &cfg,
            true,
            renderer.themes(),
            vec!["tuxedo".into()],
            wb,
            None,
            None,
        );
        st.push_banner(AlertLevel::Warn, "Non-fatal banner test");
        st.push_toast(AlertLevel::Info, "Toast test", 50_000);
        st.open_import_modal(sample_png, None).unwrap();

        let ctx = egui::Context::default();
        configure_visuals(&ctx);

        let lf = renderer.compose_logical(&scene, None, 800, 500, 50_000);
        let color_img = rgba_to_color_image(lf.img);
        let logical_size = (lf.img.width(), lf.img.height());
        let backdrop = lf.backdrop;
        let tex = ctx.load_texture("test_office", color_img, pixel_texture_options());

        for tab in [DockTab::Agents, DockTab::Whiteboard, DockTab::CeoOutfit, DockTab::Inbox] {
            st.active_tab = tab;
            let raw_input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 760.0))),
                ..Default::default()
            };
            let output = ctx.run(raw_input, |ctx| {
                let vf = ViewportFrame {
                    texture: &tex,
                    logical_size,
                    backdrop,
                };
                let (_actions, vp) = draw_desktop_ui(ctx, &mut st, &scene, Some(vf), 50_000);
                assert!(vp.avail_points.0 > 100.0 && vp.avail_points.1 > 100.0);
            });
            assert!(!output.shapes.is_empty());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
