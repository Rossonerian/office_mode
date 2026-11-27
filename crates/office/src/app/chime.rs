//! Audio chimes: which event plays which sound, rate limiting, pure-Rust WAV synthesis,
//! and cross-platform non-blocking playback.
//! The decision logic is pure and tested with a fake player; tests never play real sound.

use office_core::assets::{validate_wav_bytes, ImportLimits};
use office_core::config::SoundsConfig;
use office_core::paths;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};

/// Minimum gap between two chimes.
pub const CHIME_INTERVAL_MS: u64 = 3_000;

/// Bounded worker queue capacity; excess chimes are dropped without blocking the UI.
const CHIME_QUEUE_CAP: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chime {
    /// Played when `m` turns sound on, to prove it works.
    Test,
    /// An agent now needs you.
    NeedsYou,
    /// A result or plan landed in your boss inbox.
    Inbox,
    /// A critical alert, or an escalation in the inbox.
    Error,
}

impl Chime {
    /// macOS system sound name for this event.
    pub fn sound_name(self) -> &'static str {
        match self {
            Chime::Test => "Pop",
            Chime::NeedsYou => "Glass",
            Chime::Inbox => "Ping",
            Chime::Error => "Basso",
        }
    }

    /// Canonical lowercase slot name used for cached synthesized WAV files.
    #[cfg_attr(windows, allow(dead_code))]
    fn slot_name(self) -> &'static str {
        match self {
            Chime::Test => "test",
            Chime::NeedsYou => "needs-you",
            Chime::Inbox => "inbox",
            Chime::Error => "error",
        }
    }

    fn custom_slot(self, cfg: &SoundsConfig) -> Option<&str> {
        let raw = match self {
            Chime::Test => cfg.test.as_deref(),
            Chime::NeedsYou => cfg.needs_you.as_deref(),
            Chime::Inbox => cfg.inbox.as_deref(),
            Chime::Error => cfg.error.as_deref(),
        };
        raw.map(str::trim).filter(|s| !s.is_empty())
    }
}

/// Pure-Rust 16-bit mono 44.1 kHz PCM RIFF/WAVE synthesizer with distinct pleasant tones per chime.
pub fn synthesize_wav(chime: Chime) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    let samples: Vec<i16> = match chime {
        Chime::Test => {
            // Short upward pop/chirp (520 Hz -> 980 Hz over 90 ms).
            synth_sweep(SAMPLE_RATE, 90, 520.0, 980.0, 0.36)
        }
        Chime::NeedsYou => {
            // Crisp two-note ascending chime (E5 -> A5).
            let mut out = synth_tone(SAMPLE_RATE, 110, 659.25, 1318.5, 0.34);
            out.extend_from_slice(&vec![0i16; (SAMPLE_RATE as usize * 20) / 1000]);
            out.extend_from_slice(&synth_tone(SAMPLE_RATE, 180, 880.0, 1760.0, 0.36));
            out
        }
        Chime::Inbox => {
            // Warm bell-like ping (C6 with soft overtone over 220 ms).
            synth_tone(SAMPLE_RATE, 220, 1046.5, 1567.98, 0.32)
        }
        Chime::Error => {
            // Low descending double-tone (220 Hz -> 165 Hz).
            let mut out = synth_tone(SAMPLE_RATE, 110, 220.0, 330.0, 0.38);
            out.extend_from_slice(&vec![0i16; (SAMPLE_RATE as usize * 40) / 1000]);
            out.extend_from_slice(&synth_tone(SAMPLE_RATE, 160, 165.0, 247.5, 0.38));
            out
        }
    };
    encode_pcm16_mono_wav(SAMPLE_RATE, &samples)
}

fn synth_tone(sample_rate: u32, duration_ms: usize, freq_hz: f64, overtone_hz: f64, gain: f64) -> Vec<i16> {
    let n = (sample_rate as usize * duration_ms) / 1000;
    let attack = (sample_rate as usize * 6) / 1000;
    let tau = (duration_ms as f64 / 1000.0) * 0.42;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / sample_rate as f64;
        let env_attack = if attack > 0 && i < attack { i as f64 / attack as f64 } else { 1.0 };
        let env_decay = (-t / tau).exp();
        let env_tail = if i + attack > n { (n - i) as f64 / attack.max(1) as f64 } else { 1.0 };
        let env = env_attack * env_decay * env_tail;
        let wave = 0.80 * (std::f64::consts::TAU * freq_hz * t).sin()
            + 0.20 * (std::f64::consts::TAU * overtone_hz * t).sin();
        let sample = (wave * env * gain * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(sample);
    }
    out
}

fn synth_sweep(sample_rate: u32, duration_ms: usize, start_hz: f64, end_hz: f64, gain: f64) -> Vec<i16> {
    let n = (sample_rate as usize * duration_ms) / 1000;
    let dur_s = duration_ms as f64 / 1000.0;
    let attack = (sample_rate as usize * 4) / 1000;
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let frac = i as f64 / n.max(1) as f64;
        let freq = start_hz + (end_hz - start_hz) * frac;
        phase += std::f64::consts::TAU * freq / sample_rate as f64;
        let env_attack = if attack > 0 && i < attack { i as f64 / attack as f64 } else { 1.0 };
        let env_decay = (-(frac * dur_s) / (dur_s * 0.45)).exp();
        let env_tail = if i + attack > n { (n - i) as f64 / attack.max(1) as f64 } else { 1.0 };
        let sample = (phase.sin() * env_attack * env_decay * env_tail * gain * i16::MAX as f64)
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(sample);
    }
    out
}

fn encode_pcm16_mono_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36u32.saturating_add(data_bytes);
    let byte_rate = sample_rate * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Anything that can make a sound (real: `SystemPlayer` / `AfPlayer`; tests: a recording fake).
#[allow(dead_code)]
pub trait SoundPlayer {
    fn play(&mut self, chime: Chime);
}

struct ChimeRequest {
    chime: Chime,
    mode: String,
    custom_wav: Option<PathBuf>,
}

/// Validate a custom WAV configured for a chime slot: must be a relative path strictly confined
/// inside canonical `sounds_dir`, a regular non-symlink file, and a valid WAV <= 4 MiB.
pub(crate) fn validate_custom_wav_in(sounds_dir: &Path, configured: &str) -> Result<PathBuf, String> {
    if configured.is_empty() {
        return Err("empty custom WAV path".into());
    }
    let mut rel = PathBuf::new();
    let mut has_normal = false;
    for comp in Path::new(configured).components() {
        match comp {
            Component::Normal(part) => {
                rel.push(part);
                has_normal = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("custom WAV path escapes sounds dir: {configured:?}"));
            }
        }
    }
    if !has_normal {
        return Err(format!("custom WAV path names no file: {configured:?}"));
    }

    let root_meta = std::fs::symlink_metadata(sounds_dir)
        .map_err(|e| format!("sounds dir unavailable ({}): {e}", sounds_dir.display()))?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(format!("sounds dir is not a real directory: {}", sounds_dir.display()));
    }
    let canon_root = std::fs::canonicalize(sounds_dir)
        .map_err(|e| format!("cannot canonicalize sounds dir {}: {e}", sounds_dir.display()))?;

    // Check every path component along the way so no intermediate symlink can be traversed.
    let mut cur = sounds_dir.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        let meta = std::fs::symlink_metadata(&cur)
            .map_err(|e| format!("cannot stat custom WAV {}: {e}", cur.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("refusing symlink in custom WAV path: {}", cur.display()));
        }
    }

    let target = sounds_dir.join(&rel);
    let meta = std::fs::symlink_metadata(&target)
        .map_err(|e| format!("cannot stat custom WAV {}: {e}", target.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("custom WAV is not a regular file: {}", target.display()));
    }

    let canon_target = std::fs::canonicalize(&target)
        .map_err(|e| format!("cannot canonicalize custom WAV {}: {e}", target.display()))?;
    if !canon_target.starts_with(&canon_root) {
        return Err(format!("custom WAV escapes sounds dir: {}", canon_target.display()));
    }

    let limits = ImportLimits::default();
    if meta.len() > limits.max_wav_bytes as u64 {
        return Err(format!(
            "custom WAV {} ({} bytes) exceeds limit {} bytes",
            canon_target.display(),
            meta.len(),
            limits.max_wav_bytes
        ));
    }

    let bytes = read_wav_bounded(&canon_target, limits.max_wav_bytes)?;
    validate_wav_bytes(&bytes, &limits)?;
    Ok(canon_target)
}

fn read_wav_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let f = File::open(path).map_err(|e| format!("cannot open WAV {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read WAV {}: {e}", path.display()))?;
    if buf.len() > max_bytes {
        return Err(format!("WAV {} exceeds limit {max_bytes} bytes", path.display()));
    }
    Ok(buf)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "linux")]
const LINUX_PLAYERS: [&str; 4] = ["paplay", "pw-play", "aplay", "canberra-gtk-play"];

fn check_backend_available() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/afplay").is_file() || find_in_path("afplay").is_some() {
            Ok(())
        } else {
            Err("macOS audio backend `afplay` not found".into())
        }
    }
    #[cfg(target_os = "linux")]
    {
        if LINUX_PLAYERS.iter().any(|p| find_in_path(p).is_some()) {
            Ok(())
        } else {
            Err(format!(
                "no Linux audio player found in PATH (tried: {})",
                LINUX_PLAYERS.join(", ")
            ))
        }
    }
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Err("no audio backend available on this platform".into())
    }
}

/// Cross-platform audio chime player backed by a bounded background worker thread.
pub struct SystemPlayer {
    cfg: SoundsConfig,
    tx: SyncSender<ChimeRequest>,
}

impl Default for SystemPlayer {
    fn default() -> Self {
        Self::new(SoundsConfig::default())
    }
}

impl SystemPlayer {
    pub fn new(cfg: SoundsConfig) -> Self {
        let (tx, rx) = mpsc::sync_channel::<ChimeRequest>(CHIME_QUEUE_CAP);
        let _ = std::thread::Builder::new().name("chime-player".into()).spawn(move || {
            while let Ok(req) = rx.recv() {
                play_on_worker(req);
            }
        });
        Self { cfg, tx }
    }

    /// Synchronously verify audio backend availability and any configured custom WAV,
    /// then enqueue playback onto the bounded worker thread without blocking the UI.
    pub fn play_checked(&mut self, chime: Chime) -> Result<(), String> {
        check_backend_available()?;
        let custom_wav = match chime.custom_slot(&self.cfg) {
            Some(rel) => Some(validate_custom_wav_in(&paths::sounds_dir(), rel)?),
            None => None,
        };
        let req = ChimeRequest {
            chime,
            mode: self.cfg.mode.clone(),
            custom_wav,
        };
        // Non-blocking: drop excess chimes if the worker queue is saturated.
        let _ = self.tx.try_send(req);
        Ok(())
    }
}

impl SoundPlayer for SystemPlayer {
    fn play(&mut self, chime: Chime) {
        let _ = self.play_checked(chime);
    }
}

/// Legacy unit-struct adapter delegating to a shared `SystemPlayer` so existing callers
/// (`chime::AfPlayer.play(chime)`) keep working seamlessly.
#[allow(dead_code)]
pub struct AfPlayer;

impl SoundPlayer for AfPlayer {
    fn play(&mut self, chime: Chime) {
        static PLAYER: OnceLock<Mutex<SystemPlayer>> = OnceLock::new();
        let m = PLAYER.get_or_init(|| Mutex::new(SystemPlayer::default()));
        if let Ok(mut p) = m.lock() {
            p.play(chime);
        }
    }
}

#[cfg(unix)]
fn ensure_cached_synth_wav(chime: Chime) -> Result<PathBuf, String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = paths::ensure_runtime_dir().map_err(|e| format!("cannot create runtime dir: {e}"))?;
    let path = dir.join(format!("chime-{}.wav", chime.slot_name()));

    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
        Ok(mut f) => {
            let wav = synthesize_wav(chime);
            if let Err(e) = f.write_all(&wav).and_then(|()| f.sync_all()) {
                let _ = std::fs::remove_file(&path);
                return Err(format!("failed to write cached WAV {}: {e}", path.display()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|err| format!("cannot stat cached WAV {}: {err}", path.display()))?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(format!("refusing non-regular cached WAV {}", path.display()));
            }
        }
        Err(e) => return Err(format!("failed to create cached WAV {}: {e}", path.display())),
    }

    std::fs::canonicalize(&path).map_err(|e| format!("cannot canonicalize cached WAV {}: {e}", path.display()))
}

#[cfg(target_os = "macos")]
fn run_afplay(canonical_path: &Path) {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("afplay");
    // Note: macOS `afplay` rejects `--`; `canonical_path` is always an absolute path starting with `/`.
    cmd.arg(canonical_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0); // detach from terminal job control (ctrl-c, resize)
    if let Ok(mut child) = cmd.spawn() {
        let _ = child.wait();
    }
}

#[cfg(target_os = "linux")]
fn run_linux_player(canonical_path: &Path) {
    use std::process::{Command, Stdio};

    for player in LINUX_PLAYERS {
        let Some(bin) = find_in_path(player) else { continue };
        let mut cmd = Command::new(bin);
        match player {
            "canberra-gtk-play" => {
                cmd.arg("-f").arg(canonical_path);
            }
            "aplay" => {
                cmd.arg("-q").arg("--").arg(canonical_path);
            }
            _ => {
                cmd.arg("--").arg(canonical_path);
            }
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = cmd.spawn() {
            let _ = child.wait();
            return;
        }
    }
}

#[cfg(windows)]
fn play_wav_bytes_win(wav: &[u8]) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
    if wav.len() < 12 {
        return;
    }
    // SAFETY: `SND_SYNC` blocks until playback completes, so `wav` remains valid for the entire call.
    unsafe {
        PlaySoundW(
            wav.as_ptr() as *const u16,
            std::ptr::null_mut(),
            SND_MEMORY | SND_SYNC | SND_NODEFAULT,
        );
    }
}

fn play_on_worker(req: ChimeRequest) {
    #[cfg(target_os = "macos")]
    {
        if let Some(custom) = req.custom_wav {
            run_afplay(&custom);
            return;
        }
        if req.mode == "system" {
            let dir = Path::new("/System/Library/Sounds");
            let candidates = [req.chime.sound_name(), "Glass", "Ping", "Pop", "Basso"];
            if let Some(canon) = candidates
                .iter()
                .map(|n| dir.join(format!("{n}.aiff")))
                .find_map(|p| std::fs::canonicalize(&p).ok().filter(|c| c.is_file()))
            {
                run_afplay(&canon);
                return;
            }
        }
        if let Ok(path) = ensure_cached_synth_wav(req.chime) {
            run_afplay(&path);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = &req.mode;
        if let Some(custom) = req.custom_wav {
            run_linux_player(&custom);
            return;
        }
        if let Ok(path) = ensure_cached_synth_wav(req.chime) {
            run_linux_player(&path);
        }
    }
    #[cfg(windows)]
    {
        let _ = &req.mode;
        if let Some(custom) = req.custom_wav {
            if let Ok(bytes) = read_wav_bounded(&custom, ImportLimits::default().max_wav_bytes) {
                play_wav_bytes_win(&bytes);
            }
            return;
        }
        let wav = synthesize_wav(req.chime);
        play_wav_bytes_win(&wav);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = req;
    }
}

/// Decides whether a chime plays now: needs sound enabled and at least 3 s since the last one.
#[derive(Debug, Default)]
pub struct Chimer {
    last_ms: Option<u64>,
}

impl Chimer {
    /// Returns true when the chime should play (and records it). `Chime::Test` ignores the rate limit.
    pub fn allow(&mut self, enabled: bool, chime: Chime, now_ms: u64) -> bool {
        if !enabled {
            return false;
        }
        if chime != Chime::Test {
            if let Some(last) = self.last_ms {
                if now_ms < last.saturating_add(CHIME_INTERVAL_MS) {
                    return false;
                }
            }
        }
        self.last_ms = Some(now_ms);
        true
    }

    /// Decide and play in one step.
    #[cfg(test)]
    pub fn request(&mut self, enabled: bool, chime: Chime, now_ms: u64, player: &mut dyn SoundPlayer) -> bool {
        let ok = self.allow(enabled, chime, now_ms);
        if ok {
            player.play(chime);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct Fake(Vec<Chime>);
    impl SoundPlayer for Fake {
        fn play(&mut self, c: Chime) {
            self.0.push(c);
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "office-chime-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn events_map_to_distinct_sounds() {
        let names: Vec<_> = [Chime::Test, Chime::NeedsYou, Chime::Inbox, Chime::Error].iter().map(|c| c.sound_name()).collect();
        assert_eq!(names, ["Pop", "Glass", "Ping", "Basso"]);
    }

    #[test]
    fn disabled_never_plays() {
        let (mut c, mut p) = (Chimer::default(), Fake::default());
        assert!(!c.request(false, Chime::Error, 10_000, &mut p));
        assert!(!c.request(false, Chime::Test, 20_000, &mut p));
        assert!(p.0.is_empty());
    }

    #[test]
    fn rate_limited_to_one_per_three_seconds() {
        let (mut c, mut p) = (Chimer::default(), Fake::default());
        assert!(c.request(true, Chime::NeedsYou, 10_000, &mut p));
        assert!(!c.request(true, Chime::Error, 12_999, &mut p));
        assert!(c.request(true, Chime::Inbox, 13_000, &mut p));
        assert_eq!(p.0, vec![Chime::NeedsYou, Chime::Inbox]);
    }

    #[test]
    fn test_chime_bypasses_limit_and_restarts_it() {
        let (mut c, mut p) = (Chimer::default(), Fake::default());
        assert!(c.request(true, Chime::NeedsYou, 10_000, &mut p));
        assert!(c.request(true, Chime::Test, 10_500, &mut p));
        assert!(!c.request(true, Chime::Inbox, 12_000, &mut p));
        assert_eq!(p.0, vec![Chime::NeedsYou, Chime::Test]);
    }

    #[test]
    fn synthesize_wav_produces_valid_distinct_pcm_wavs() {
        let limits = ImportLimits::default();
        let all = [
            synthesize_wav(Chime::Test),
            synthesize_wav(Chime::NeedsYou),
            synthesize_wav(Chime::Inbox),
            synthesize_wav(Chime::Error),
        ];
        for wav in &all {
            assert!(validate_wav_bytes(wav, &limits).is_ok());
            assert!(wav.len() > 44);
            // 16-bit mono 44.1kHz PCM header checks
            assert_eq!(&wav[12..16], b"fmt ");
            assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
            assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
            assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 44_100);
            assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
            assert_eq!(&wav[36..40], b"data");
        }
        // Every chime variant produces a distinct waveform.
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "chimes {i} and {j} should be distinct");
            }
        }
    }

    #[test]
    fn custom_wav_validation_rejects_traversal_symlinks_and_bad_headers() {
        let dir = temp_dir("custom-wav");
        let sounds = dir.join("sounds");
        std::fs::create_dir_all(&sounds).unwrap();

        let valid_wav = synthesize_wav(Chime::Inbox);
        let good = sounds.join("inbox.wav");
        std::fs::write(&good, &valid_wav).unwrap();
        let outside = dir.join("outside.wav");
        std::fs::write(&outside, &valid_wav).unwrap();
        let bad = sounds.join("corrupt.wav");
        std::fs::write(&bad, b"not a wav file at all").unwrap();

        // Valid relative file inside sounds_dir succeeds.
        let resolved = validate_custom_wav_in(&sounds, "inbox.wav").unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&good).unwrap());

        // Traversal and absolute paths are rejected.
        assert!(validate_custom_wav_in(&sounds, "../outside.wav").is_err());
        assert!(validate_custom_wav_in(&sounds, &outside.to_string_lossy()).is_err());
        assert!(validate_custom_wav_in(&sounds, "").is_err());
        assert!(validate_custom_wav_in(&sounds, "missing.wav").is_err());
        assert!(validate_custom_wav_in(&sounds, "corrupt.wav").is_err());

        // Symlinks inside sounds_dir are rejected.
        #[cfg(unix)]
        {
            let link = sounds.join("escape.wav");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(validate_custom_wav_in(&sounds, "escape.wav").is_err());

            let dir_link = sounds.join("linked_dir");
            std::os::unix::fs::symlink(&dir, &dir_link).unwrap();
            assert!(validate_custom_wav_in(&sounds, "linked_dir/outside.wav").is_err());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
