//! Safe validation and atomic import of user asset packs (themes, sprite overrides, CEO outfits, sounds).
//! Pure Rust validation (no `image` crate dependency): inspects PNG IHDR and WAV RIFF/WAVE headers directly.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Canonical logical sprite names shared across `office-core` and `office`.
pub const SPRITE_NAMES: [&str; 14] = [
    "floor",
    "wall",
    "window",
    "door",
    "desk",
    "monitor_off",
    "monitor_on",
    "chair",
    "plant",
    "sofa",
    "cooler",
    "whiteboard",
    "lamp",
    "rug",
];

/// Valid sound slot names for `import_sound`.
pub const SOUND_SLOTS: [&str; 4] = ["test", "needs-you", "inbox", "error"];

/// Safety caps applied to all asset imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLimits {
    pub max_toml_bytes: usize,
    pub max_png_bytes: usize,
    pub max_png_dim: u32,
    pub max_wav_bytes: usize,
    pub max_pack_bytes: u64,
    pub max_pack_files: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_toml_bytes: 256 * 1024,
            max_png_bytes: 8 * 1024 * 1024,
            max_png_dim: 4096,
            max_wav_bytes: 4 * 1024 * 1024,
            max_pack_bytes: 32 * 1024 * 1024,
            max_pack_files: 64,
        }
    }
}

fn default_outfit_sprite() -> String {
    "outfit.png".into()
}

/// Manifest stored in `<outfits_dir>/<name>/outfit.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OutfitManifest {
    pub name: Option<String>,
    /// `"torso"` | `"full"`
    pub mode: Option<String>,
    #[serde(default = "default_outfit_sprite")]
    pub sprite: String,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
}

impl Default for OutfitManifest {
    fn default() -> Self {
        Self {
            name: None,
            mode: None,
            sprite: default_outfit_sprite(),
            primary_color: None,
            secondary_color: None,
        }
    }
}

/// Enforce `^[a-z0-9][a-z0-9_-]{0,47}$` for pack/theme/outfit names.
pub fn sanitize_pack_name(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() || bytes.len() > 48 {
        return Err(format!(
            "invalid pack name {raw:?}: must be 1..=48 chars matching ^[a-z0-9][a-z0-9_-]{{0,47}}$"
        ));
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!(
            "invalid pack name {raw:?}: must start with lowercase ASCII letter or digit"
        ));
    }
    for &b in &bytes[1..] {
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            return Err(format!(
                "invalid pack name {raw:?}: contains disallowed character {:?}",
                b as char
            ));
        }
    }
    Ok(raw.to_string())
}

/// Validate raw PNG bytes by checking size cap, 8-byte PNG signature, and IHDR width/height.
pub fn validate_png_bytes(bytes: &[u8], limits: &ImportLimits) -> Result<(u32, u32), String> {
    if bytes.len() > limits.max_png_bytes {
        return Err(format!(
            "PNG size {} exceeds limit {} bytes",
            bytes.len(),
            limits.max_png_bytes
        ));
    }
    // 8-byte signature + 4-byte length + 4-byte "IHDR" + 13-byte IHDR data + 4-byte CRC = 33 bytes minimum
    if bytes.len() < 33 {
        return Err("PNG file too short to contain valid IHDR".into());
    }
    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes[0..8] != PNG_SIG {
        return Err("invalid PNG signature".into());
    }
    let ihdr_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if ihdr_len != 13 {
        return Err(format!("invalid PNG IHDR length {ihdr_len} (expected 13)"));
    }
    if &bytes[12..16] != b"IHDR" {
        return Err("first PNG chunk is not IHDR".into());
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        return Err(format!("invalid PNG dimensions {w}x{h}"));
    }
    if w > limits.max_png_dim || h > limits.max_png_dim {
        return Err(format!(
            "PNG dimensions {w}x{h} exceed limit {}x{}",
            limits.max_png_dim, limits.max_png_dim
        ));
    }
    Ok((w, h))
}

/// Validate raw WAV bytes by checking size cap and RIFF/WAVE header.
pub fn validate_wav_bytes(bytes: &[u8], limits: &ImportLimits) -> Result<(), String> {
    if bytes.len() < 12 {
        return Err("WAV file too short (expected at least 12 bytes)".into());
    }
    if bytes.len() > limits.max_wav_bytes {
        return Err(format!(
            "WAV size {} exceeds limit {} bytes",
            bytes.len(),
            limits.max_wav_bytes
        ));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("invalid WAV header (expected RIFF....WAVE)".into());
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<std::fs::Metadata, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("refusing symlink: {}", path.display()));
    }
    Ok(meta)
}

fn reject_existing_symlink(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(format!("refusing symlink destination: {}", path.display()))
        }
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot stat {}: {e}", path.display())),
    }
}

fn validate_rel_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("empty relative path".into());
    }
    let p = Path::new(raw);
    let mut out = PathBuf::new();
    let mut has_normal = false;
    for comp in p.components() {
        match comp {
            Component::Normal(part) => {
                out.push(part);
                has_normal = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("disallowed path traversal or absolute path: {raw:?}"));
            }
        }
    }
    if !has_normal {
        return Err(format!("path names no file: {raw:?}"));
    }
    Ok(out)
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let meta = reject_symlink(path)?;
    if !meta.is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    if meta.len() > max_bytes as u64 {
        return Err(format!(
            "file {} ({} bytes) exceeds limit {} bytes",
            path.display(),
            meta.len(),
            max_bytes
        ));
    }
    let f = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut buf = Vec::with_capacity((meta.len() as usize).min(max_bytes));
    f.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if buf.len() > max_bytes {
        return Err(format!(
            "file {} exceeds limit {} bytes",
            path.display(),
            max_bytes
        ));
    }
    Ok(buf)
}

fn next_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Safely replace `dest` with `staging` (which must live in `dest.parent()`).
/// If `dest` exists, renames it to a sibling backup first and restores on failure.
pub fn commit_dir_swap(staging: &Path, dest: &Path) -> Result<(), String> {
    let dest_parent = dest
        .parent()
        .ok_or_else(|| format!("destination has no parent: {}", dest.display()))?;
    let stage_parent = staging
        .parent()
        .ok_or_else(|| format!("staging dir has no parent: {}", staging.display()))?;
    if stage_parent != dest_parent {
        return Err(format!(
            "staging dir {} must share parent with destination {}",
            staging.display(),
            dest.display()
        ));
    }

    let exists = reject_existing_symlink(dest)?;
    if exists {
        let dest_name = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pack");
        let backup = dest_parent.join(format!(
            "{dest_name}.bak.{}.{}",
            std::process::id(),
            next_seq()
        ));
        if let Err(e) = std::fs::rename(dest, &backup) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(format!("failed to move existing {} aside: {e}", dest.display()));
        }
        match std::fs::rename(staging, dest) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&backup);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::rename(&backup, dest);
                let _ = std::fs::remove_dir_all(staging);
                Err(format!("failed to commit {} to {}: {e}", staging.display(), dest.display()))
            }
        }
    } else {
        if let Err(e) = std::fs::rename(staging, dest) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(format!("failed to commit {} to {}: {e}", staging.display(), dest.display()));
        }
        Ok(())
    }
}

fn collect_pack_files(
    root: &Path,
    rel: &Path,
    limits: &ImportLimits,
    total_files: &mut usize,
    total_bytes: &mut u64,
    out: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<(), String> {
    let cur = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let meta = reject_symlink(&cur)?;
    if !meta.is_dir() {
        return Err(format!("expected directory: {}", cur.display()));
    }
    let rd = std::fs::read_dir(&cur)
        .map_err(|e| format!("cannot read dir {}: {e}", cur.display()))?;
    let mut entries = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| format!("cannot read dir entry in {}: {e}", cur.display()))?;
        entries.push(entry.file_name());
    }
    entries.sort();

    for name in entries {
        let child_rel = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        let child_abs = root.join(&child_rel);
        let cmeta = reject_symlink(&child_abs)?;
        if cmeta.is_dir() {
            collect_pack_files(root, &child_rel, limits, total_files, total_bytes, out)?;
        } else if cmeta.is_file() {
            *total_files += 1;
            if *total_files > limits.max_pack_files {
                return Err(format!(
                    "pack exceeds maximum file count ({})",
                    limits.max_pack_files
                ));
            }
            let ext = child_abs
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            let per_file_cap = match ext.as_deref() {
                Some("png") => limits.max_png_bytes,
                Some("toml") => limits.max_toml_bytes,
                _ => (limits.max_pack_bytes as usize).min(limits.max_png_bytes),
            };
            let bytes = read_bounded(&child_abs, per_file_cap)?;
            *total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            if *total_bytes > limits.max_pack_bytes {
                return Err(format!(
                    "pack total size exceeds limit {} bytes",
                    limits.max_pack_bytes
                ));
            }
            if ext.as_deref() == Some("png") {
                validate_png_bytes(&bytes, limits)?;
            }
            out.push((child_rel, bytes));
        } else {
            return Err(format!("unsupported file type: {}", child_abs.display()));
        }
    }
    Ok(())
}

/// Validate and import a theme pack directory into `<themes_root>/<name>`.
pub fn import_theme_pack_to(
    src_dir: &Path,
    name: &str,
    themes_root: &Path,
) -> Result<PathBuf, String> {
    let clean_name = sanitize_pack_name(name)?;
    let limits = ImportLimits::default();

    let src_meta = reject_symlink(src_dir)?;
    if !src_meta.is_dir() {
        return Err(format!("theme source is not a directory: {}", src_dir.display()));
    }

    let toml_path = src_dir.join("theme.toml");
    let toml_bytes = read_bounded(&toml_path, limits.max_toml_bytes)?;
    let toml_str = std::str::from_utf8(&toml_bytes)
        .map_err(|e| format!("theme.toml is not valid UTF-8: {e}"))?;
    let parsed: toml::Value =
        toml::from_str(toml_str).map_err(|e| format!("invalid theme.toml: {e}"))?;
    let table = parsed
        .as_table()
        .ok_or_else(|| "theme.toml root must be a TOML table".to_string())?;

    // Validate sheet file paths (no traversal, no absolute paths, must exist as non-symlink inside pack).
    if let Some(sheets) = table.get("sheets").and_then(|v| v.as_table()) {
        for (sheet_name, val) in sheets {
            let file_str = val
                .get("file")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("sheet {sheet_name:?} missing string `file` field"))?;
            let rel = validate_rel_path(file_str)
                .map_err(|e| format!("sheet {sheet_name:?}: {e}"))?;
            let target = src_dir.join(&rel);
            let tmeta = reject_symlink(&target)
                .map_err(|e| format!("sheet {sheet_name:?}: {e}"))?;
            if !tmeta.is_file() {
                return Err(format!("sheet {sheet_name:?} file not found: {file_str:?}"));
            }
        }
    }

    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut files = Vec::new();
    collect_pack_files(
        src_dir,
        Path::new(""),
        &limits,
        &mut total_files,
        &mut total_bytes,
        &mut files,
    )?;

    std::fs::create_dir_all(themes_root)
        .map_err(|e| format!("cannot create {}: {e}", themes_root.display()))?;
    let dest = themes_root.join(&clean_name);
    let staging = themes_root.join(format!(
        ".stage.{clean_name}.{}.{}",
        std::process::id(),
        next_seq()
    ));
    std::fs::create_dir(&staging)
        .map_err(|e| format!("cannot create staging dir {}: {e}", staging.display()))?;

    for (rel, bytes) in &files {
        let out_path = staging.join(rel);
        if let Some(parent) = out_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(format!("cannot create staging subdir {}: {e}", parent.display()));
            }
        }
        if let Err(e) = crate::paths::atomic_write(&out_path, bytes) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("failed to write staged file {}: {e}", out_path.display()));
        }
    }

    commit_dir_swap(&staging, &dest)?;
    Ok(dest)
}

/// Validate and import a theme pack into `~/.config/agent-office/themes/<name>`.
pub fn import_theme_pack(src_dir: &Path, name: &str) -> Result<PathBuf, String> {
    import_theme_pack_to(src_dir, name, &crate::paths::user_themes_dir())
}

/// Import a single PNG as a sprite override into `<themes_root>/<theme>/sprites/<sprite>.png`
/// and register it in `<themes_root>/<theme>/theme.toml`.
pub fn import_sprite_override_to(
    theme: &str,
    sprite: &str,
    src_png: &Path,
    themes_root: &Path,
) -> Result<PathBuf, String> {
    let clean_theme = sanitize_pack_name(theme)?;
    if !SPRITE_NAMES.contains(&sprite) {
        return Err(format!(
            "unknown sprite {sprite:?}; allowed: {}",
            SPRITE_NAMES.join(", ")
        ));
    }
    let limits = ImportLimits::default();
    let png_bytes = read_bounded(src_png, limits.max_png_bytes)?;
    let (w, h) = validate_png_bytes(&png_bytes, &limits)?;

    let theme_dir = themes_root.join(&clean_theme);
    reject_existing_symlink(&theme_dir)?;
    let sprites_dir = theme_dir.join("sprites");
    reject_existing_symlink(&sprites_dir)?;
    let sprite_path = sprites_dir.join(format!("{sprite}.png"));
    reject_existing_symlink(&sprite_path)?;
    let theme_toml_path = theme_dir.join("theme.toml");
    let toml_exists = reject_existing_symlink(&theme_toml_path)?;

    std::fs::create_dir_all(&sprites_dir)
        .map_err(|e| format!("cannot create {}: {e}", sprites_dir.display()))?;

    crate::paths::atomic_write(&sprite_path, &png_bytes)
        .map_err(|e| format!("failed to write {}: {e}", sprite_path.display()))?;

    let mut root_table = if toml_exists {
        let existing = read_bounded(&theme_toml_path, limits.max_toml_bytes)?;
        let existing_str = std::str::from_utf8(&existing)
            .map_err(|e| format!("existing theme.toml is not UTF-8: {e}"))?;
        let val: toml::Value = toml::from_str(existing_str)
            .map_err(|e| format!("existing theme.toml is invalid: {e}"))?;
        val.as_table()
            .cloned()
            .ok_or_else(|| "existing theme.toml root must be a table".to_string())?
    } else {
        toml::map::Map::new()
    };

    let sheet_key = format!("override_{sprite}");
    let mut sheet_entry = toml::map::Map::new();
    sheet_entry.insert(
        "file".into(),
        toml::Value::String(format!("sprites/{sprite}.png")),
    );
    sheet_entry.insert("tile".into(), toml::Value::Integer(16));
    sheet_entry.insert("spacing".into(), toml::Value::Integer(0));

    let sheets_tbl = root_table
        .entry("sheets")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "theme.toml `sheets` must be a table".to_string())?;
    sheets_tbl.insert(sheet_key.clone(), toml::Value::Table(sheet_entry));

    let mut sprite_entry = toml::map::Map::new();
    sprite_entry.insert("sheet".into(), toml::Value::String(sheet_key));
    sprite_entry.insert("x".into(), toml::Value::Integer(0));
    sprite_entry.insert("y".into(), toml::Value::Integer(0));
    sprite_entry.insert("w".into(), toml::Value::Integer(w as i64));
    sprite_entry.insert("h".into(), toml::Value::Integer(h as i64));

    let sprites_tbl = root_table
        .entry("sprites")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "theme.toml `sprites` must be a table".to_string())?;
    sprites_tbl.insert(sprite.to_string(), toml::Value::Table(sprite_entry));

    let serialized = toml::to_string_pretty(&toml::Value::Table(root_table))
        .map_err(|e| format!("failed to serialize theme.toml: {e}"))?;
    crate::paths::atomic_write(&theme_toml_path, serialized.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", theme_toml_path.display()))?;

    Ok(sprite_path)
}

/// Import a single PNG as a sprite override into `~/.config/agent-office/themes/<theme>`.
pub fn import_sprite_override(theme: &str, sprite: &str, src_png: &Path) -> Result<PathBuf, String> {
    import_sprite_override_to(theme, sprite, src_png, &crate::paths::user_themes_dir())
}

fn validate_outfit_mode(m: &str) -> Result<(), String> {
    if m == "torso" || m == "full" {
        Ok(())
    } else {
        Err(format!("invalid outfit mode {m:?}: expected \"torso\" or \"full\""))
    }
}

/// Import a CEO outfit from either a single PNG file or a directory containing `outfit.toml` + sprite PNG.
pub fn import_ceo_outfit_to(
    src: &Path,
    name: &str,
    mode: Option<&str>,
    outfits_root: &Path,
) -> Result<PathBuf, String> {
    let clean_name = sanitize_pack_name(name)?;
    if let Some(m) = mode {
        validate_outfit_mode(m)?;
    }
    let limits = ImportLimits::default();
    let src_meta = reject_symlink(src)?;

    let (manifest, png_bytes) = if src_meta.is_file() {
        let bytes = read_bounded(src, limits.max_png_bytes)?;
        validate_png_bytes(&bytes, &limits)?;
        let manifest = OutfitManifest {
            name: Some(clean_name.clone()),
            mode: Some(mode.unwrap_or("torso").to_string()),
            sprite: "outfit.png".into(),
            primary_color: None,
            secondary_color: None,
        };
        (manifest, bytes)
    } else if src_meta.is_dir() {
        let toml_path = src.join("outfit.toml");
        let toml_bytes = read_bounded(&toml_path, limits.max_toml_bytes)?;
        let toml_str = std::str::from_utf8(&toml_bytes)
            .map_err(|e| format!("outfit.toml is not valid UTF-8: {e}"))?;
        let mut manifest: OutfitManifest =
            toml::from_str(toml_str).map_err(|e| format!("invalid outfit.toml: {e}"))?;
        if let Some(m) = mode {
            manifest.mode = Some(m.to_string());
        } else if let Some(ref existing_mode) = manifest.mode {
            validate_outfit_mode(existing_mode)?;
        } else {
            manifest.mode = Some("torso".into());
        }
        if manifest.name.is_none() {
            manifest.name = Some(clean_name.clone());
        }
        let rel_sprite = validate_rel_path(&manifest.sprite)
            .map_err(|e| format!("outfit.toml sprite: {e}"))?;
        let sprite_src = src.join(&rel_sprite);
        let bytes = read_bounded(&sprite_src, limits.max_png_bytes)?;
        validate_png_bytes(&bytes, &limits)?;
        manifest.sprite = "outfit.png".into();
        (manifest, bytes)
    } else {
        return Err(format!("unsupported outfit source: {}", src.display()));
    };

    std::fs::create_dir_all(outfits_root)
        .map_err(|e| format!("cannot create {}: {e}", outfits_root.display()))?;
    let dest = outfits_root.join(&clean_name);
    let staging = outfits_root.join(format!(
        ".stage.{clean_name}.{}.{}",
        std::process::id(),
        next_seq()
    ));
    std::fs::create_dir(&staging)
        .map_err(|e| format!("cannot create staging dir {}: {e}", staging.display()))?;

    let manifest_toml = toml::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize outfit.toml: {e}"))?;
    if let Err(e) = crate::paths::atomic_write(&staging.join("outfit.toml"), manifest_toml.as_bytes()) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("failed to write staged outfit.toml: {e}"));
    }
    if let Err(e) = crate::paths::atomic_write(&staging.join("outfit.png"), &png_bytes) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("failed to write staged outfit.png: {e}"));
    }

    commit_dir_swap(&staging, &dest)?;
    Ok(dest)
}

/// Import a CEO outfit into `~/.config/agent-office/outfits/<name>`.
pub fn import_ceo_outfit(src: &Path, name: &str, mode: Option<&str>) -> Result<PathBuf, String> {
    import_ceo_outfit_to(src, name, mode, &crate::paths::outfits_dir())
}

/// Import a validated WAV file into `<sounds_root>/<slot>.wav`.
pub fn import_sound_to(slot: &str, src_wav: &Path, sounds_root: &Path) -> Result<PathBuf, String> {
    if !SOUND_SLOTS.contains(&slot) {
        return Err(format!(
            "invalid sound slot {slot:?}; allowed: {}",
            SOUND_SLOTS.join(", ")
        ));
    }
    let limits = ImportLimits::default();
    let wav_bytes = read_bounded(src_wav, limits.max_wav_bytes)?;
    validate_wav_bytes(&wav_bytes, &limits)?;

    let dest = sounds_root.join(format!("{slot}.wav"));
    crate::paths::atomic_write(&dest, &wav_bytes)
        .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Import a validated WAV file into `~/.config/agent-office/sounds/<slot>.wav`.
pub fn import_sound(slot: &str, src_wav: &Path) -> Result<PathBuf, String> {
    import_sound_to(slot, src_wav, &crate::paths::sounds_dir())
}

/// List sorted valid outfit pack names in `outfits_root`.
pub fn list_outfits_in(outfits_root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(outfits_root) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if sanitize_pack_name(&name).is_err() {
            continue;
        }
        let toml_path = path.join("outfit.toml");
        if let Ok(tmeta) = std::fs::symlink_metadata(&toml_path) {
            if !tmeta.file_type().is_symlink() && tmeta.is_file() {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// List sorted valid outfit pack names in `~/.config/agent-office/outfits`.
pub fn list_outfits() -> Vec<String> {
    list_outfits_in(&crate::paths::outfits_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "office-assets-test-{tag}-{}-{}",
            std::process::id(),
            next_seq()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn minimal_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(33);
        out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, comp, filter, interlace
        out.extend_from_slice(&[0, 0, 0, 0]); // dummy CRC
        out
    }

    fn minimal_wav() -> Vec<u8> {
        let mut out = Vec::with_capacity(44);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&36u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&[0u8; 32]);
        out
    }

    #[test]
    fn sanitize_pack_name_cases() {
        assert_eq!(sanitize_pack_name("cozy").unwrap(), "cozy");
        assert_eq!(sanitize_pack_name("pack_01-v2").unwrap(), "pack_01-v2");
        assert_eq!(sanitize_pack_name("0").unwrap(), "0");
        assert_eq!(sanitize_pack_name(&"a".repeat(48)).unwrap(), "a".repeat(48));

        assert!(sanitize_pack_name("").is_err());
        assert!(sanitize_pack_name("../x").is_err());
        assert!(sanitize_pack_name("a/b").is_err());
        assert!(sanitize_pack_name("a\\b").is_err());
        assert!(sanitize_pack_name("Upper").is_err());
        assert!(sanitize_pack_name("has space").is_err());
        assert!(sanitize_pack_name("-leading").is_err());
        assert!(sanitize_pack_name("_leading").is_err());
        assert!(sanitize_pack_name(&"a".repeat(49)).is_err());
    }

    #[test]
    fn png_validation_checks_signature_ihdr_and_dims() {
        let limits = ImportLimits::default();
        assert_eq!(validate_png_bytes(&minimal_png(16, 32), &limits).unwrap(), (16, 32));
        assert_eq!(validate_png_bytes(&minimal_png(4096, 4096), &limits).unwrap(), (4096, 4096));

        assert!(validate_png_bytes(b"not a png at all", &limits).is_err());
        assert!(validate_png_bytes(&minimal_png(0, 16), &limits).is_err());
        assert!(validate_png_bytes(&minimal_png(4097, 16), &limits).is_err());
        assert!(validate_png_bytes(&minimal_png(16, 4097), &limits).is_err());

        let mut bad_chunk = minimal_png(16, 16);
        bad_chunk[12..16].copy_from_slice(b"IDAT");
        assert!(validate_png_bytes(&bad_chunk, &limits).is_err());
    }

    #[test]
    fn wav_validation_checks_header_and_size() {
        let limits = ImportLimits::default();
        assert!(validate_wav_bytes(&minimal_wav(), &limits).is_ok());
        assert!(validate_wav_bytes(b"short", &limits).is_err());
        assert!(validate_wav_bytes(b"OggS1234567890", &limits).is_err());

        let tiny_limits = ImportLimits { max_wav_bytes: 16, ..limits };
        assert!(validate_wav_bytes(&minimal_wav(), &tiny_limits).is_err());
    }

    #[test]
    fn theme_pack_import_traversal_and_reimport_swap() {
        let dir = test_dir("theme-pack");
        let src = dir.join("src_pack");
        let themes_root = dir.join("themes");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("sheet.png"), minimal_png(32, 32)).unwrap();
        std::fs::write(
            src.join("theme.toml"),
            "[sheets]\ns = { file = \"sheet.png\", tile = 16 }\n[sprites]\nfloor = { sheet = \"s\", col = 0, row = 0 }\n",
        )
        .unwrap();

        let dest = import_theme_pack_to(&src, "my-pack", &themes_root).unwrap();
        assert_eq!(dest, themes_root.join("my-pack"));
        assert!(dest.join("theme.toml").is_file());
        assert!(dest.join("sheet.png").is_file());

        // Re-import replaces cleanly via backup-swap.
        std::fs::write(src.join("extra.png"), minimal_png(16, 16)).unwrap();
        let dest2 = import_theme_pack_to(&src, "my-pack", &themes_root).unwrap();
        assert_eq!(dest2, dest);
        assert!(dest2.join("extra.png").is_file());

        // Traversal in theme.toml sheet `file` is rejected without clobbering existing pack.
        let bad_src = dir.join("bad_pack");
        std::fs::create_dir_all(&bad_src).unwrap();
        std::fs::write(
            bad_src.join("theme.toml"),
            "[sheets]\ns = { file = \"../evil.png\", tile = 16 }\n",
        )
        .unwrap();
        assert!(import_theme_pack_to(&bad_src, "my-pack", &themes_root).is_err());
        assert!(dest.join("extra.png").is_file(), "existing pack preserved on failed re-import");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn theme_pack_rejects_symlinks() {
        let dir = test_dir("theme-symlink");
        let src = dir.join("src_pack");
        let themes_root = dir.join("themes");
        let outside = dir.join("outside.png");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(&outside, minimal_png(16, 16)).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("sheet.png")).unwrap();
        std::fs::write(
            src.join("theme.toml"),
            "[sheets]\ns = { file = \"sheet.png\", tile = 16 }\n",
        )
        .unwrap();

        assert!(import_theme_pack_to(&src, "sym-pack", &themes_root).is_err());
        assert!(!themes_root.join("sym-pack").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sprite_override_allowlist_and_valid_import() {
        let dir = test_dir("sprite-override");
        let themes_root = dir.join("themes");
        let png = dir.join("desk.png");
        std::fs::write(&png, minimal_png(32, 16)).unwrap();

        assert!(import_sprite_override_to("custom", "invalid_sprite", &png, &themes_root).is_err());

        let out = import_sprite_override_to("custom", "desk", &png, &themes_root).unwrap();
        assert_eq!(out, themes_root.join("custom/sprites/desk.png"));
        assert!(out.is_file());

        let toml_text = std::fs::read_to_string(themes_root.join("custom/theme.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&toml_text).unwrap();
        assert_eq!(
            parsed["sheets"]["override_desk"]["file"].as_str(),
            Some("sprites/desk.png")
        );
        assert_eq!(parsed["sprites"]["desk"]["w"].as_integer(), Some(32));
        assert_eq!(parsed["sprites"]["desk"]["h"].as_integer(), Some(16));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ceo_outfit_single_png_and_dir_import() {
        let dir = test_dir("outfit");
        let outfits_root = dir.join("outfits");
        let png = dir.join("jersey.png");
        std::fs::write(&png, minimal_png(16, 24)).unwrap();

        // Single PNG import
        let dest1 = import_ceo_outfit_to(&png, "jersey-9", Some("torso"), &outfits_root).unwrap();
        assert!(dest1.join("outfit.toml").is_file());
        assert!(dest1.join("outfit.png").is_file());

        // Directory import
        let pack_src = dir.join("tux_pack");
        std::fs::create_dir_all(&pack_src).unwrap();
        std::fs::write(pack_src.join("tux.png"), minimal_png(16, 32)).unwrap();
        std::fs::write(
            pack_src.join("outfit.toml"),
            "name = \"Tuxedo\"\nmode = \"full\"\nsprite = \"tux.png\"\nprimary_color = \"#111111\"\n",
        )
        .unwrap();
        let dest2 = import_ceo_outfit_to(&pack_src, "tuxedo", None, &outfits_root).unwrap();
        assert!(dest2.join("outfit.toml").is_file());
        assert!(dest2.join("outfit.png").is_file());
        let manifest: OutfitManifest =
            toml::from_str(&std::fs::read_to_string(dest2.join("outfit.toml")).unwrap()).unwrap();
        assert_eq!(manifest.mode.as_deref(), Some("full"));
        assert_eq!(manifest.sprite, "outfit.png");

        // Traversal in outfit.toml sprite rejected
        let bad_pack = dir.join("bad_pack");
        std::fs::create_dir_all(&bad_pack).unwrap();
        std::fs::write(bad_pack.join("outfit.toml"), "sprite = \"../jersey.png\"\n").unwrap();
        assert!(import_ceo_outfit_to(&bad_pack, "bad", None, &outfits_root).is_err());

        // list_outfits_in returns sorted valid packs
        assert_eq!(list_outfits_in(&outfits_root), vec!["jersey-9", "tuxedo"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sound_import_valid_and_invalid_slot() {
        let dir = test_dir("sound");
        let sounds_root = dir.join("sounds");
        let wav = dir.join("ping.wav");
        std::fs::write(&wav, minimal_wav()).unwrap();

        assert!(import_sound_to("bogus-slot", &wav, &sounds_root).is_err());
        let dest = import_sound_to("needs-you", &wav, &sounds_root).unwrap();
        assert_eq!(dest, sounds_root.join("needs-you.wav"));
        assert_eq!(std::fs::read(&dest).unwrap(), minimal_wav());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
