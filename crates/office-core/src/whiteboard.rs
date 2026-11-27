//! Persistent office whiteboard task list with optional ROSSO wave sync.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_WHITEBOARD_BYTES: usize = 256 * 1024;
pub const MAX_WHITEBOARD_ITEMS: usize = 256;
pub const MAX_TITLE_CHARS: usize = 200;

/// Sanitize a task title: collapses newlines/tabs to spaces, strips control characters,
/// trims leading/trailing whitespace, and caps at `MAX_TITLE_CHARS` characters.
pub fn sanitize_title(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_TITLE_CHARS));
    let mut prev_space = false;
    for ch in raw.chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else if ch.is_control() {
            continue;
        } else {
            prev_space = ch == ' ';
            out.push(ch);
        }
    }
    out.trim().chars().take(MAX_TITLE_CHARS).collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskSource {
    Manual,
    Wave,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub source: TaskSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Whiteboard {
    pub items: Vec<TaskItem>,
    pub sync_waves: bool,
}

impl Default for Whiteboard {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            sync_waves: true,
        }
    }
}

fn next_manual_id(existing: &[TaskItem]) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    loop {
        let candidate = format!("m-{}", SEQ.fetch_add(1, Ordering::Relaxed));
        if !existing.iter().any(|i| i.id == candidate) {
            return candidate;
        }
    }
}

fn corrupt_backup_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".corrupt");
    PathBuf::from(os)
}

fn is_done_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "done" | "ok" | "pass" | "passed" | "completed" | "approved"
    )
}

impl Whiteboard {
    /// Rounded completion percentage `0..=100` (0 when empty).
    pub fn completion_pct(&self) -> u8 {
        let (done, total) = self.counts();
        if total == 0 {
            return 0;
        }
        (((done * 100) + total / 2) / total).min(100) as u8
    }

    /// `(done, total)` item counts.
    pub fn counts(&self) -> (usize, usize) {
        let done = self.items.iter().filter(|i| i.done).count();
        (done, self.items.len())
    }

    /// Add a new manual task. Returns `false` if empty after sanitization or at `MAX_WHITEBOARD_ITEMS`.
    pub fn add(&mut self, title: &str) -> bool {
        if self.items.len() >= MAX_WHITEBOARD_ITEMS {
            return false;
        }
        let clean = sanitize_title(title);
        if clean.is_empty() {
            return false;
        }
        let id = next_manual_id(&self.items);
        self.items.push(TaskItem {
            id,
            title: clean,
            done: false,
            source: TaskSource::Manual,
        });
        true
    }

    /// Toggle `done` state of item at `idx`.
    pub fn toggle(&mut self, idx: usize) -> bool {
        if let Some(item) = self.items.get_mut(idx) {
            item.done = !item.done;
            true
        } else {
            false
        }
    }

    /// Remove item at `idx`.
    pub fn remove(&mut self, idx: usize) -> bool {
        if idx < self.items.len() {
            self.items.remove(idx);
            true
        } else {
            false
        }
    }

    /// Remove completed `Manual` items; returns removed count.
    pub fn clear_done(&mut self) -> usize {
        let before = self.items.len();
        self.items
            .retain(|i| !(i.done && i.source == TaskSource::Manual));
        before - self.items.len()
    }

    /// When `sync_waves` is enabled, sync wave steps from `scene.waves` (and `scene.auto_jobs`
    /// not already present in `scene.waves`) into `TaskSource::Wave` items while preserving
    /// `TaskSource::Manual` items and respecting `MAX_WHITEBOARD_ITEMS`.
    /// Returns `true` if `items` changed.
    pub fn sync_from_scene(&mut self, scene: &crate::model::Scene) -> bool {
        if !self.sync_waves {
            return false;
        }

        let mut next: Vec<TaskItem> = self
            .items
            .iter()
            .filter(|i| i.source == TaskSource::Manual)
            .take(MAX_WHITEBOARD_ITEMS)
            .cloned()
            .collect();

        for (job_name, wave) in &scene.waves {
            for (idx, call) in wave.calls.iter().enumerate() {
                if next.len() >= MAX_WHITEBOARD_ITEMS {
                    break;
                }
                let raw_title = if call.title.trim().is_empty() {
                    job_name.clone()
                } else {
                    format!("{job_name}: {}", call.title)
                };
                let title = sanitize_title(&raw_title);
                if title.is_empty() {
                    continue;
                }
                next.push(TaskItem {
                    id: format!("w-{job_name}-{idx}"),
                    title,
                    done: is_done_status(&call.status),
                    source: TaskSource::Wave,
                });
            }
        }

        for aj in &scene.auto_jobs {
            if next.len() >= MAX_WHITEBOARD_ITEMS {
                break;
            }
            if scene.waves.contains_key(&aj.job) {
                continue;
            }
            let raw_title = if aj.task.trim().is_empty() {
                format!("{} ({})", aj.job, aj.phase)
            } else {
                format!("{}: {} ({})", aj.job, aj.task, aj.phase)
            };
            let title = sanitize_title(&raw_title);
            if title.is_empty() {
                continue;
            }
            next.push(TaskItem {
                id: format!("a-{}", aj.job),
                title,
                done: is_done_status(&aj.status),
                source: TaskSource::Wave,
            });
        }

        if next != self.items {
            self.items = next;
            true
        } else {
            false
        }
    }

    /// Load whiteboard JSON from `path`.
    /// - Missing file -> `(Self::default(), None)`.
    /// - Oversized (> `MAX_WHITEBOARD_BYTES`) or corrupt JSON -> best-effort copy to `<path>.corrupt`
    ///   without overwriting `path`, returning `(Self::default(), Some(warning))`.
    /// - Valid JSON -> sanitizes all titles and truncates to `MAX_WHITEBOARD_ITEMS`.
    pub fn load_from(path: &Path) -> (Self, Option<String>) {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("failed to stat {}: {e}", path.display())),
                );
            }
        };

        if meta.file_type().is_symlink() {
            return (
                Self::default(),
                Some(format!("refusing to read symlink whiteboard: {}", path.display())),
            );
        }

        if meta.len() > MAX_WHITEBOARD_BYTES as u64 {
            let backup = corrupt_backup_path(path);
            let _ = std::fs::copy(path, &backup);
            return (
                Self::default(),
                Some(format!(
                    "whiteboard exceeds {MAX_WHITEBOARD_BYTES} bytes; backed up to {}",
                    backup.display()
                )),
            );
        }

        let mut bytes = Vec::with_capacity(meta.len() as usize);
        let read_res = File::open(path).and_then(|f| {
            f.take((MAX_WHITEBOARD_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
        });
        if let Err(e) = read_res {
            return (
                Self::default(),
                Some(format!("failed to read {}: {e}", path.display())),
            );
        }
        if bytes.len() > MAX_WHITEBOARD_BYTES {
            let backup = corrupt_backup_path(path);
            let _ = std::fs::copy(path, &backup);
            return (
                Self::default(),
                Some(format!(
                    "whiteboard exceeds {MAX_WHITEBOARD_BYTES} bytes; backed up to {}",
                    backup.display()
                )),
            );
        }

        match serde_json::from_slice::<Whiteboard>(&bytes) {
            Ok(mut wb) => {
                wb.items.truncate(MAX_WHITEBOARD_ITEMS);
                for item in &mut wb.items {
                    item.title = sanitize_title(&item.title);
                }
                (wb, None)
            }
            Err(e) => {
                let backup = corrupt_backup_path(path);
                let _ = std::fs::copy(path, &backup);
                (
                    Self::default(),
                    Some(format!(
                        "corrupt whiteboard JSON ({e}); backed up to {}",
                        backup.display()
                    )),
                )
            }
        }
    }

    /// Persist whiteboard JSON atomically via `paths::atomic_write`.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::paths::atomic_write(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AutoJob, Scene, WaveCall, WaveState};

    fn test_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "office-whiteboard-test-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn completion_pct_and_counts() {
        let mut wb = Whiteboard::default();
        assert_eq!(wb.completion_pct(), 0);
        assert_eq!(wb.counts(), (0, 0));

        assert!(wb.add("task 1"));
        assert!(wb.add("task 2"));
        assert!(wb.add("task 3"));
        assert_eq!(wb.completion_pct(), 0);

        assert!(wb.toggle(0));
        assert_eq!(wb.counts(), (1, 3));
        assert_eq!(wb.completion_pct(), 33);

        assert!(wb.toggle(1));
        assert_eq!(wb.completion_pct(), 67);

        assert!(wb.toggle(2));
        assert_eq!(wb.completion_pct(), 100);

        assert_eq!(wb.clear_done(), 3);
        assert_eq!(wb.counts(), (0, 0));
    }

    #[test]
    fn sanitize_title_strips_controls_and_caps_200_chars() {
        let raw = "  hello\t\nworld\x1b[31m\x00!  ";
        assert_eq!(sanitize_title(raw), "hello world[31m!");

        let long = "x".repeat(300);
        let clean = sanitize_title(&long);
        assert_eq!(clean.chars().count(), MAX_TITLE_CHARS);

        assert_eq!(sanitize_title("\n\t\x00\x07   "), "");
    }

    #[test]
    fn enforces_max_whiteboard_items_cap() {
        let mut wb = Whiteboard::default();
        for i in 0..MAX_WHITEBOARD_ITEMS {
            assert!(wb.add(&format!("item {i}")));
        }
        assert_eq!(wb.items.len(), MAX_WHITEBOARD_ITEMS);
        assert!(!wb.add("one too many"));
        assert!(!wb.add("   \n\t  "));
    }

    #[test]
    fn sync_from_scene_preserves_manual_and_syncs_waves() {
        let mut wb = Whiteboard::default();
        assert!(wb.add("Manual note"));
        wb.toggle(0);

        let mut scene = Scene::default();
        scene.waves.insert(
            "job1".into(),
            WaveState {
                job: "job1".into(),
                plan: "plan".into(),
                updated_ms: 100,
                done: 1,
                relay_inflight: 0,
                pending: 1,
                calls: vec![
                    WaveCall {
                        title: "Stage 1\nbuild".into(),
                        status: "done".into(),
                        verdict: Some("PASS".into()),
                        relay: false,
                    },
                    WaveCall {
                        title: "Stage 2".into(),
                        status: "running".into(),
                        verdict: None,
                        relay: false,
                    },
                ],
            },
        );
        scene.auto_jobs.push(AutoJob {
            job: "auto2".into(),
            task: "Ship feature".into(),
            phase: "test".into(),
            status: "running".into(),
            ..Default::default()
        });

        assert!(wb.sync_from_scene(&scene));
        assert_eq!(wb.items.len(), 4);
        assert_eq!(wb.items[0].source, TaskSource::Manual);
        assert_eq!(wb.items[0].title, "Manual note");
        assert!(wb.items[0].done);

        assert_eq!(wb.items[1].source, TaskSource::Wave);
        assert_eq!(wb.items[1].title, "job1: Stage 1 build");
        assert!(wb.items[1].done);

        assert_eq!(wb.items[2].source, TaskSource::Wave);
        assert_eq!(wb.items[2].title, "job1: Stage 2");
        assert!(!wb.items[2].done);

        assert_eq!(wb.items[3].source, TaskSource::Wave);
        assert_eq!(wb.items[3].title, "auto2: Ship feature (test)");
        assert!(!wb.items[3].done);

        // Second sync with same scene reports no change.
        assert!(!wb.sync_from_scene(&scene));

        // clear_done only removes completed Manual items, keeping Wave items.
        assert_eq!(wb.clear_done(), 1);
        assert_eq!(wb.items.len(), 3);
        assert!(wb.items.iter().all(|i| i.source == TaskSource::Wave));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = test_dir("roundtrip");
        let path = dir.join("whiteboard.json");

        // Missing file -> default, no warning.
        let (empty, warn) = Whiteboard::load_from(&path);
        assert_eq!(empty, Whiteboard::default());
        assert!(warn.is_none());

        let mut wb = Whiteboard::default();
        wb.add("Write tests");
        wb.add("Ship release");
        wb.toggle(0);
        wb.save_to(&path).unwrap();

        let (loaded, warn2) = Whiteboard::load_from(&path);
        assert!(warn2.is_none());
        assert_eq!(loaded, wb);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_json_backs_up_without_clobbering_original() {
        let dir = test_dir("corrupt");
        let path = dir.join("whiteboard.json");
        let original = b"{ not valid json !!!";
        std::fs::write(&path, original).unwrap();

        let (wb, warn) = Whiteboard::load_from(&path);
        assert_eq!(wb, Whiteboard::default());
        assert!(warn.is_some());

        // Original file is NOT overwritten.
        assert_eq!(std::fs::read(&path).unwrap(), original);

        // Backup file exists with original content.
        let backup = dir.join("whiteboard.json.corrupt");
        assert_eq!(std::fs::read(&backup).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
