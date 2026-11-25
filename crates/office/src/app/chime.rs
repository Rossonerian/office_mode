//! Audio chimes: which event plays which sound, rate limiting, and the (non-blocking) macOS player.
//! The decision logic is pure and tested with a fake player; tests never play real sound.
use std::process::{Command, Stdio};

/// Minimum gap between two chimes.
pub const CHIME_INTERVAL_MS: u64 = 3_000;

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
}

/// Anything that can make a sound (real: `AfPlayer`; tests: a recording fake).
pub trait SoundPlayer {
    fn play(&mut self, chime: Chime);
}

/// Plays `/System/Library/Sounds/<name>.aiff` with `afplay`, detached, never blocking the UI.
pub struct AfPlayer;

impl SoundPlayer for AfPlayer {
    fn play(&mut self, chime: Chime) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let dir = "/System/Library/Sounds";
        // Fall back to other system sounds if the preferred one is missing.
        let candidates = [chime.sound_name(), "Glass", "Ping", "Pop", "Basso"];
        let Some(path) = candidates.iter().map(|n| format!("{dir}/{n}.aiff")).find(|p| std::path::Path::new(p).exists()) else {
            return;
        };
        let mut cmd = Command::new("afplay");
        cmd.arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // detach from the terminal's job control (ctrl-c, resize)
        }
        if let Ok(mut child) = cmd.spawn() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
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

    #[derive(Default)]
    struct Fake(Vec<Chime>);
    impl SoundPlayer for Fake {
        fn play(&mut self, c: Chime) {
            self.0.push(c);
        }
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
}
