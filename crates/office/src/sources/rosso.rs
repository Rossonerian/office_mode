//! ROSSO watcher: tails every recent job's `logbook.jsonl` (deliveries) and `mailroom.jsonl`
//! (escalations, refusals) under `~/.claude/run/rosso/` and emits `EventKind::RossoMail`.
use office_core::model::{Alert, AlertLevel};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

const POLL_EVERY: Duration = Duration::from_secs(2);
const TRAY_EVERY: Duration = Duration::from_secs(5);
const MAX_JOB_AGE: Duration = Duration::from_secs(2 * 3600);
const MAX_BODY_CHARS: usize = 16_000;

/// `$OFFICE_ROSSO_DIR`, else `~/.claude/run/rosso`.
pub fn rosso_dir() -> PathBuf {
    match std::env::var_os("OFFICE_ROSSO_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => paths::claude_dir().join("run").join("rosso"),
    }
}

pub fn spawn(_cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = rosso_dir();
    std::thread::Builder::new().name("rosso".into()).spawn(move || {
        let mut tails = Tails::default();
        let mut initial = true;
        let mut last_trays: Option<std::time::Instant> = None;
        loop {
            let mut evs = poll(&dir, &mut tails, initial);
            if last_trays.map_or(true, |t| t.elapsed() >= TRAY_EVERY) {
                last_trays = Some(std::time::Instant::now());
                evs.extend(scan_in_trays(&dir).into_iter().map(|(slug, count)| EventKind::InTray { slug, count }));
            }
            for k in evs {
                if tx.send(Event::now(SourceKind::Rosso, k)).is_err() {
                    return;
                }
            }
            initial = false;
            std::thread::sleep(POLL_EVERY);
        }
    })?;
    Ok(())
}

#[derive(Default)]
pub struct Tails {
    offsets: HashMap<PathBuf, u64>,
}

#[derive(Clone, Copy, PartialEq)]
enum FileKind {
    Logbook,
    Mailroom,
}

/// One scan of the rosso dir: new lines since the last scan, as events. Files first seen during
/// the `initial` scan start at their end (no history replay); files that appear later start at 0.
pub fn poll(dir: &Path, tails: &mut Tails, initial: bool) -> Vec<EventKind> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let jd = e.path();
        if !jd.is_dir() {
            continue;
        }
        let fresh = std::fs::metadata(jd.join("job.json"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|age| age <= MAX_JOB_AGE)
            .unwrap_or(false);
        if !fresh {
            continue;
        }
        let job = e.file_name().to_string_lossy().into_owned();
        for (name, fk) in [("logbook.jsonl", FileKind::Logbook), ("mailroom.jsonl", FileKind::Mailroom)] {
            let path = jd.join(name);
            for line in new_lines(&path, tails, initial) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(k) = line_event(&job, fk, &v) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// In-tray counts: files in `<job>/desks/<slug>/received/` of every job updated within the last
/// two hours. When several jobs have a desk with the same slug, the freshest job wins. Slugs that
/// had a tray but are empty now report 0, so a badge clears.
pub fn scan_in_trays(dir: &Path) -> Vec<(String, u32)> {
    let mut best: HashMap<String, (SystemTime, u32)> = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    for e in rd.flatten() {
        let jd = e.path();
        let Some(mtime) = std::fs::metadata(jd.join("job.json")).and_then(|m| m.modified()).ok() else { continue };
        if SystemTime::now().duration_since(mtime).map(|a| a > MAX_JOB_AGE).unwrap_or(false) {
            continue;
        }
        let Ok(desks) = std::fs::read_dir(jd.join("desks")) else { continue };
        for d in desks.flatten().take(64) {
            let slug = d.file_name().to_string_lossy().into_owned();
            // read_dir follows nothing: symlink entries inside received/ are counted as entries
            let n = std::fs::read_dir(d.path().join("received")).map(|r| r.flatten().count().min(99) as u32).unwrap_or(0);
            if best.get(&slug).map_or(true, |(t, _)| mtime > *t) {
                best.insert(slug, (mtime, n));
            }
        }
    }
    let mut v: Vec<(String, u32)> = best.into_iter().map(|(k, (_, n))| (k, n)).collect();
    v.sort();
    v
}

fn new_lines(path: &Path, tails: &mut Tails, initial: bool) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = match tails.offsets.get(path) {
        None if initial => {
            tails.offsets.insert(path.to_path_buf(), len);
            return Vec::new();
        }
        None => 0,
        Some(&o) if o > len => 0,
        Some(&o) => o,
    };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(4 << 20).read_to_end(&mut buf);
    // only complete lines; leave a partial tail for the next scan
    let used = buf.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
    tails.offsets.insert(path.to_path_buf(), start + used as u64);
    String::from_utf8_lossy(&buf[..used]).lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|x| !x.is_empty())
}

fn titles(v: &Value) -> Vec<String> {
    match v.get("to") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(t)) if !t.is_empty() => vec![t.clone()],
        _ => Vec::new(),
    }
}

fn line_event(job: &str, fk: FileKind, v: &Value) -> Option<EventKind> {
    match fk {
        FileKind::Logbook => {
            let doc_path = s(v, "doc").map(str::to_string);
            let body = doc_path
                .as_deref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.chars().take(MAX_BODY_CHARS).collect::<String>())
                .unwrap_or_default();
            Some(EventKind::RossoMail {
                job: job.to_string(),
                from_title: s(v, "from")?.to_string(),
                to_titles: titles(v),
                doc_type: s(v, "type").unwrap_or("Document").to_string(),
                verdict: s(v, "verdict").map(str::to_string),
                subject: s(v, "subject").or_else(|| s(v, "ref")).unwrap_or("").to_string(),
                doc_path,
                escalation: false,
                body,
            })
        }
        FileKind::Mailroom => match s(v, "event")? {
            "escalation" => Some(EventKind::RossoMail {
                job: job.to_string(),
                from_title: s(v, "from")?.to_string(),
                to_titles: titles(v),
                doc_type: "Escalation".into(),
                verdict: None,
                subject: s(v, "ref").unwrap_or("").to_string(),
                doc_path: None,
                escalation: true,
                body: String::new(),
            }),
            "refused" => Some(EventKind::Alert(Alert {
                at_ms: paths::now_ms(),
                level: AlertLevel::Warn,
                agent: None,
                code: "rosso_refused".into(),
                message: format!(
                    "rosso refused {} from {}: {}",
                    s(v, "type").unwrap_or("?"),
                    s(v, "from").unwrap_or("?"),
                    s(v, "reason").unwrap_or("?")
                ),
            })),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tails_new_lines_only() {
        let d = std::env::temp_dir().join(format!("office-rosso-{}", std::process::id()));
        let job = d.join("j1");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("job.json"), "{}").unwrap();
        let doc = job.join("d.md");
        std::fs::write(&doc, "hello body").unwrap();
        let lb = job.join("logbook.jsonl");
        std::fs::write(&lb, "{\"from\":\"Old\",\"to\":[\"CEO\"],\"type\":\"Memo\"}\n").unwrap();
        let mut t = Tails::default();
        assert!(poll(&d, &mut t, true).is_empty());
        let mut f = std::fs::OpenOptions::new().append(true).open(&lb).unwrap();
        writeln!(f, "{{\"from\":\"Test Inspector\",\"to\":[\"Chief of Staff\"],\"type\":\"Inspection Report\",\"verdict\":\"PASS\",\"subject\":\"S\",\"doc\":\"{}\"}}", doc.display()).unwrap();
        write!(f, "{{\"from\":\"partial").unwrap();
        let mut mr = std::fs::File::create(job.join("mailroom.jsonl")).unwrap();
        writeln!(mr, "{{\"event\":\"escalation\",\"ref\":\"R-1\",\"from\":\"Security Seat\",\"to\":[\"CEO\"]}}").unwrap();
        writeln!(mr, "{{\"event\":\"refused\",\"from\":\"X\",\"type\":\"Critique\",\"reason\":\"no\"}}").unwrap();
        let evs = poll(&d, &mut t, false);
        assert_eq!(evs.len(), 3, "{evs:?}");
        let mut seen = 0;
        for k in &evs {
            match k {
                EventKind::RossoMail { from_title, escalation: false, body, verdict, to_titles, .. } => {
                    assert_eq!(from_title, "Test Inspector");
                    assert_eq!(body, "hello body");
                    assert_eq!(verdict.as_deref(), Some("PASS"));
                    assert_eq!(to_titles, &vec!["Chief of Staff".to_string()]);
                    seen += 1;
                }
                EventKind::RossoMail { escalation: true, doc_type, subject, .. } => {
                    assert_eq!((doc_type.as_str(), subject.as_str()), ("Escalation", "R-1"));
                    seen += 1;
                }
                EventKind::Alert(a) => {
                    assert!(a.message.contains("refused Critique from X"));
                    seen += 1;
                }
                _ => {}
            }
        }
        assert_eq!(seen, 3);
        assert!(poll(&d, &mut t, false).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn counts_in_tray_per_desk_freshest_job_wins() {
        let d = std::env::temp_dir().join(format!("office-trays-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let j = d.join("job-a");
        for (slug, n) in [("chief-inspector", 3), ("technician", 0)] {
            let r = j.join("desks").join(slug).join("received");
            std::fs::create_dir_all(&r).unwrap();
            for i in 0..n {
                std::fs::write(r.join(format!("doc{i}.md")), "x").unwrap();
            }
        }
        std::fs::create_dir_all(j.join("desks/chief-of-staff/authored")).unwrap();
        std::fs::write(j.join("job.json"), "{}").unwrap();
        // a stale job is ignored
        let old = d.join("old");
        std::fs::create_dir_all(old.join("desks/ghost/received")).unwrap();
        std::fs::write(old.join("desks/ghost/received/x"), "x").unwrap();
        std::fs::write(old.join("job.json"), "{}").unwrap();
        let f = std::fs::File::options().write(true).open(old.join("job.json")).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(3 * 3600)).unwrap();
        let v = scan_in_trays(&d);
        assert_eq!(v, vec![("chief-inspector".to_string(), 3), ("chief-of-staff".to_string(), 0), ("technician".to_string(), 0)]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
