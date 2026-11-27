//! ROSSO watcher: tails every recent job's `logbook.jsonl` (deliveries) and `mailroom.jsonl`
//! (escalations, refusals) under `~/.claude/run/rosso/` and emits `EventKind::RossoMail`.
use office_core::model::{Alert, AlertLevel, AutoJob, ReportKind, WaveCall, WaveState};
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        let mut last_waves: BTreeMap<String, WaveState> = BTreeMap::new();
        let mut last_auto_jobs: Vec<AutoJob> = Vec::new();
        let mut seen_auto_docs: HashMap<PathBuf, (u64, u64)> = HashMap::new();
        let mut auto_initial = true;
        loop {
            let mut evs = poll(&dir, &mut tails, initial);
            if last_trays.is_none_or(|t| t.elapsed() >= TRAY_EVERY) {
                last_trays = Some(std::time::Instant::now());
                evs.extend(scan_in_trays(&dir).into_iter().map(|(slug, count)| EventKind::InTray { slug, count }));
                let waves = scan_waves(&dir, paths::now_ms());
                if waves != last_waves {
                    last_waves = waves.clone();
                    evs.push(EventKind::Waves(waves));
                }
                let auto_jobs = scan_auto_jobs(&dir);
                evs.extend(auto_doc_events(&dir, &auto_jobs, &mut seen_auto_docs, auto_initial));
                auto_initial = false;
                if auto_jobs != last_auto_jobs {
                    last_auto_jobs = auto_jobs.clone();
                    evs.push(EventKind::AutoJobs(auto_jobs));
                }
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
            if best.get(&slug).is_none_or(|(t, _)| mtime > *t) {
                best.insert(slug, (mtime, n));
            }
        }
    }
    let mut v: Vec<(String, u32)> = best.into_iter().map(|(k, (_, n))| (k, n)).collect();
    v.sort();
    v
}

const RELAY_TTL_MS: u64 = 900_000;

fn is_leap_year(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 using Howard Hinnant's civil-from-days / days-from-civil integer math.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj as i64 + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an ISO-8601 UTC timestamp (`"YYYY-MM-DDTHH:MM:SS(.fff)Z"`) into epoch milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b't')
        || b[13] != b':'
        || b[16] != b':'
        || (*b.last()? != b'Z' && *b.last()? != b'z')
    {
        return None;
    }
    let parse_u = |slice: &str| -> Option<u32> {
        if !slice.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        slice.parse().ok()
    };
    let year = parse_u(&s[0..4])?;
    let month = parse_u(&s[5..7])?;
    let day = parse_u(&s[8..10])?;
    let hour = parse_u(&s[11..13])?;
    let min = parse_u(&s[14..16])?;
    let sec = parse_u(&s[17..19])?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let tail = &s[19..s.len() - 1];
    let ms: u64 = if tail.is_empty() {
        0
    } else {
        let frac = tail.strip_prefix('.')?;
        if frac.is_empty() || !frac.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut mul = 100u64;
        let mut acc = 0u64;
        for ch in frac.bytes().take(3) {
            acc += (ch - b'0') as u64 * mul;
            mul /= 10;
        }
        acc
    };
    let days = days_from_civil(year as i64, month, day);
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400_000 + (hour as u64 * 3600 + min as u64 * 60 + sec as u64) * 1000 + ms)
}

fn is_fresh_time(t: SystemTime) -> bool {
    SystemTime::now().duration_since(t).map(|age| age <= MAX_JOB_AGE).unwrap_or(false)
}

fn mtime_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn read_json_bounded(path: &Path) -> Option<Value> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(1 << 20).read_to_end(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn clean_verdict(v: Option<&str>) -> Option<String> {
    let v = v?.trim();
    if v.is_empty() || v == "-" || v.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(v.to_string())
    }
}

/// Scan active ROSSO job directories under `dir` and return the freshest wave snapshot per job.
pub fn scan_waves(dir: &Path, now_ms: u64) -> BTreeMap<String, WaveState> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten().take(256) {
        let jd = e.path();
        if !jd.is_dir() {
            continue;
        }
        let job_json_path = jd.join("job.json");
        let job_fresh = std::fs::metadata(&job_json_path).and_then(|m| m.modified()).ok().is_some_and(is_fresh_time);
        let runs_dir = jd.join("runs");
        let Ok(runs_rd) = std::fs::read_dir(&runs_dir) else { continue };

        let mut any_state_fresh = false;
        let mut best: Option<(u64, u64, String, Value)> = None;
        for re in runs_rd.flatten().take(256) {
            let fname = re.file_name().to_string_lossy().into_owned();
            let Some(plan) = fname.strip_suffix(".state.json").filter(|p| !p.is_empty()) else {
                continue;
            };
            let Ok(mtime) = std::fs::metadata(re.path()).and_then(|m| m.modified()) else { continue };
            if is_fresh_time(mtime) {
                any_state_fresh = true;
            }
            let Some(state_val) = read_json_bounded(&re.path()) else { continue };
            let fm_ms = mtime_ms(mtime);
            let updated_ms = state_val
                .get("updated")
                .and_then(|u| u.as_u64().or_else(|| u.as_str().and_then(parse_iso_ms)))
                .unwrap_or(fm_ms);
            let replace = best.as_ref().is_none_or(|(bu, bf, _, _)| (updated_ms, fm_ms) > (*bu, *bf));
            if replace {
                best = Some((updated_ms, fm_ms, plan.to_string(), state_val));
            }
        }

        if !(job_fresh || any_state_fresh) {
            continue;
        }
        let Some((updated_ms, _, plan, state_val)) = best else { continue };

        let mut calls = Vec::new();
        let done_obj = state_val.get("done").and_then(Value::as_object);
        let done = done_obj.map(|m| m.len() as u32).unwrap_or(0);
        if let Some(m) = done_obj {
            for (title, entry) in m {
                calls.push(WaveCall {
                    title: title.clone(),
                    status: s(entry, "status").unwrap_or("OK").to_string(),
                    verdict: clean_verdict(s(entry, "verdict")),
                    relay: false,
                });
            }
        }

        let mut relay_inflight = 0u32;
        if let Some(relay_obj) = state_val.get("relay").and_then(Value::as_object) {
            for (title, entry) in relay_obj {
                if done_obj.is_some_and(|d| d.contains_key(title)) {
                    continue;
                }
                let Some(issued_ms) = s(entry, "issued").and_then(parse_iso_ms) else { continue };
                if now_ms.saturating_sub(issued_ms) < RELAY_TTL_MS {
                    calls.push(WaveCall {
                        title: title.clone(),
                        status: "RELAY".into(),
                        verdict: None,
                        relay: true,
                    });
                    relay_inflight += 1;
                }
            }
        }

        let accounted = done.saturating_add(relay_inflight);
        let sibling_total = read_json_bounded(&runs_dir.join(format!("{plan}.json")))
            .and_then(|v| v.get("calls").and_then(Value::as_array).map(|a| a.len() as u32));
        let pending = if let Some(total) = sibling_total {
            total.saturating_sub(accounted)
        } else if plan == "council-r2" {
            let seats = read_json_bounded(&job_json_path)
                .and_then(|v| {
                    v.get("participants").and_then(Value::as_array).map(|arr| {
                        arr.iter().filter(|x| x.as_str().is_some_and(|p| p.ends_with(" Seat"))).count() as u32
                    })
                })
                .unwrap_or(0);
            seats.saturating_sub(accounted)
        } else if plan == "inspect" {
            2u32.saturating_sub(accounted)
        } else {
            0
        };

        calls.sort_by(|a, b| b.relay.cmp(&a.relay).then_with(|| a.title.cmp(&b.title)));
        calls.truncate(16);

        let job = e.file_name().to_string_lossy().into_owned();
        out.insert(
            job.clone(),
            WaveState {
                job,
                plan,
                updated_ms,
                done,
                relay_inflight,
                pending,
                calls,
            },
        );
    }
    out
}

fn u32_field(v: &Value, k: &str) -> u32 {
    v.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

fn read_doc_bounded(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(128 * 1024).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).chars().take(MAX_BODY_CHARS).collect())
}

/// Scan `<rosso_dir>/<job>/auto.json` for `rosso-auto` autonomous pipeline snapshots.
pub fn scan_auto_jobs(rosso_dir: &Path) -> Vec<AutoJob> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(rosso_dir) else { return out };
    for e in rd.flatten().take(256) {
        let jd = e.path();
        if !jd.is_dir() {
            continue;
        }
        let Some(v) = read_json_bounded(&jd.join("auto.json")) else { continue };
        let dir_name = e.file_name().to_string_lossy().into_owned();
        let job = s(&v, "job").unwrap_or(&dir_name).to_string();
        let started_at = v
            .get("startedAt")
            .and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(parse_iso_ms)))
            .unwrap_or(0);
        out.push(AutoJob {
            job,
            task: s(&v, "task").unwrap_or("").to_string(),
            flow: s(&v, "flow").unwrap_or("").to_string(),
            phase: s(&v, "phase").unwrap_or("").to_string(),
            status: s(&v, "status").unwrap_or("").to_string(),
            calls_used: u32_field(&v, "callsUsed"),
            max_calls: u32_field(&v, "maxCalls"),
            started_at,
            elapsed_base_ms: v.get("elapsedBaseMs").and_then(Value::as_u64).unwrap_or(0),
            max_minutes: u32_field(&v, "maxMinutes"),
            driver_pid: v.get("driverPid").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()),
            abort_requested: v.get("abortRequested").and_then(Value::as_bool).unwrap_or(false),
            active_calls: v.get("activeCalls").and_then(Value::as_object).map(|m| m.len()).unwrap_or(0),
            ruling: v.get("arbitration").and_then(|a| s(a, "ruling")).map(str::to_string),
            branch: s(&v, "branch").map(str::to_string),
            worktree_path: s(&v, "worktreePath").map(str::to_string),
            report_path: s(&v, "reportPath").map(str::to_string),
            needs_path: s(&v, "needsPath").map(str::to_string),
            last_error: s(&v, "lastError").map(str::to_string),
        });
    }
    out.sort_by(|a, b| {
        (b.status == "running")
            .cmp(&(a.status == "running"))
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.job.cmp(&b.job))
    });
    out
}

/// Scan `REPORT.md` / `NEEDS_USER_INPUT.md` for `AutoJob`s, emitting Boss Inbox `Report` handoffs
/// and non-initial `Alert`s while deduplicating unchanged files across polls.
pub fn auto_doc_events(
    rosso_dir: &Path,
    jobs: &[AutoJob],
    seen: &mut HashMap<PathBuf, (u64, u64)>,
    initial: bool,
) -> Vec<EventKind> {
    let mut out = Vec::new();
    for j in jobs {
        let report_path = j
            .report_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| rosso_dir.join(&j.job).join("REPORT.md"));
        let needs_path = j
            .needs_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| rosso_dir.join(&j.job).join("NEEDS_USER_INPUT.md"));

        for (path, is_needs) in [(report_path, false), (needs_path, true)] {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().ok();
            let sig = (modified.map(mtime_ms).unwrap_or(0), meta.len());
            if seen.get(&path) == Some(&sig) {
                continue;
            }
            seen.insert(path.clone(), sig);

            if initial && !modified.is_some_and(is_fresh_time) {
                continue;
            }
            let Some(body) = read_doc_bounded(&path) else { continue };
            let title = if is_needs {
                format!("rosso-auto {}: NEEDS_USER_INPUT", j.job)
            } else if let Some(ruling) = j.ruling.as_deref().filter(|s| !s.is_empty()) {
                format!("rosso-auto {}: REPORT ({ruling})", j.job)
            } else if !j.status.is_empty() {
                format!("rosso-auto {}: REPORT ({})", j.job, j.status)
            } else {
                format!("rosso-auto {}: REPORT", j.job)
            };
            out.push(EventKind::Handoff {
                from: format!("rosso-auto:{}", j.job),
                to: None,
                kind: ReportKind::Report,
                title,
                body,
                brief: j.task.clone(),
            });
            if !initial {
                let (level, code, message) = if is_needs {
                    let phase = if j.phase.is_empty() { "stopped" } else { &j.phase };
                    (
                        AlertLevel::Warn,
                        "rosso_needs_input",
                        format!("rosso-auto {}: needs user input ({phase})", j.job),
                    )
                } else {
                    let level = if j.status == "done" { AlertLevel::Info } else { AlertLevel::Warn };
                    let status = if j.status.is_empty() { "done" } else { &j.status };
                    (
                        level,
                        "rosso_report",
                        format!("rosso-auto {}: report ready ({status})", j.job),
                    )
                };
                out.push(EventKind::Alert(Alert {
                    at_ms: paths::now_ms(),
                    level,
                    agent: None,
                    code: code.into(),
                    message,
                }));
            }
        }
    }
    if seen.len() > 512 {
        let keep: std::collections::HashSet<&str> = jobs.iter().map(|j| j.job.as_str()).collect();
        seen.retain(|p, _| p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()).is_some_and(|n| keep.contains(n)));
    }
    out
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
        let entry = serde_json::json!({
            "from": "Test Inspector",
            "to": ["Chief of Staff"],
            "type": "Inspection Report",
            "verdict": "PASS",
            "subject": "S",
            "doc": doc.to_string_lossy(),
        });
        writeln!(f, "{entry}").unwrap();
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

    #[test]
    fn parses_iso_utc_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01.25Z"), Some(1250));
        assert_eq!(parse_iso_ms("1970-01-02T03:04:05.006Z"), Some(86_400_000 + (3 * 3600 + 4 * 60 + 5) * 1000 + 6));
        // 2020-02-29 leap day is valid; 2021-02-29 is invalid
        assert_eq!(parse_iso_ms("2020-02-29T12:00:00Z"), Some(18_321 * 86_400_000 + 12 * 3600 * 1000));
        assert_eq!(parse_iso_ms("2021-02-29T12:00:00Z"), None);
        assert_eq!(parse_iso_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso_ms("2026-01-01T00:00:00"), None);
        assert_eq!(parse_iso_ms("2026-01-01T00:00:00.Z"), None);
        assert_eq!(parse_iso_ms("1969-12-31T23:59:59Z"), None);
    }

    #[test]
    fn scans_waves_verdicts_relays_pending_and_freshest_plan() {
        let d = std::env::temp_dir().join(format!("office-waves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);

        // Job 1: freshest .state.json wins, verdicts stripped, active vs expired relay, sibling <plan>.json pending
        let j1 = d.join("job-sibling");
        std::fs::create_dir_all(j1.join("runs")).unwrap();
        std::fs::write(j1.join("job.json"), "{}").unwrap();
        std::fs::write(
            j1.join("runs/old-plan.state.json"),
            r#"{"updated":"2026-11-25T15:00:00Z","done":{"Old":{"status":"OK"}}}"#,
        )
        .unwrap();
        std::fs::write(
            j1.join("runs/smoke-plan.json"),
            r#"{"calls":[{},{},{},{},{}]}"#,
        )
        .unwrap();
        std::fs::write(
            j1.join("runs/smoke-plan.state.json"),
            r#"{
                "updated":"2026-11-25T15:10:00Z",
                "done":{
                    "Technician":{"status":"OK","verdict":"PASS"},
                    "Reviewer":{"status":"OK","verdict":"-"},
                    "Auditor":{"status":"OK","verdict":"none"},
                    "DoneAlready":{"status":"OK","verdict":""}
                },
                "relay":{
                    "ActiveRelay":{"issued":"2026-11-25T15:05:00Z"},
                    "ExpiredRelay":{"issued":"2026-11-25T14:50:00Z"},
                    "DoneAlready":{"issued":"2026-11-25T15:08:00Z"}
                }
            }"#,
        )
        .unwrap();

        // Job 2: council-r2 uses job.json participants ending with " Seat"
        let j2 = d.join("job-council");
        std::fs::create_dir_all(j2.join("runs")).unwrap();
        std::fs::write(
            j2.join("job.json"),
            r#"{"participants":["Chief of Staff","Security Seat","Reliability Seat","Product Seat"]}"#,
        )
        .unwrap();
        std::fs::write(
            j2.join("runs/council-r2.state.json"),
            r#"{"updated":"2026-11-25T15:09:00Z","done":{"Security Seat":{"status":"OK","verdict":"APPROVE"}}}"#,
        )
        .unwrap();

        // Job 3: stale job is ignored
        let j3 = d.join("job-stale");
        std::fs::create_dir_all(j3.join("runs")).unwrap();
        std::fs::write(j3.join("job.json"), "{}").unwrap();
        std::fs::write(j3.join("runs/inspect.state.json"), r#"{"done":{}}"#).unwrap();
        for p in [j3.join("job.json"), j3.join("runs/inspect.state.json")] {
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(3 * 3600)).unwrap();
        }

        let now_ms = parse_iso_ms("2026-11-25T15:11:00Z").unwrap();
        let waves = scan_waves(&d, now_ms);
        assert_eq!(waves.len(), 2);

        let w1 = waves.get("job-sibling").unwrap();
        assert_eq!(w1.plan, "smoke-plan");
        assert_eq!(w1.updated_ms, parse_iso_ms("2026-11-25T15:10:00Z").unwrap());
        assert_eq!(w1.done, 4);
        assert_eq!(w1.relay_inflight, 1);
        assert_eq!(w1.pending, 0); // 5 calls - (4 done + 1 relay_inflight) = 0
        // Relay call sorted first, then done calls sorted by title
        assert!(w1.calls[0].relay);
        assert_eq!(w1.calls[0].title, "ActiveRelay");
        assert_eq!(w1.calls[0].status, "RELAY");
        let by_title: HashMap<&str, &WaveCall> = w1.calls.iter().map(|c| (c.title.as_str(), c)).collect();
        assert_eq!(by_title["Technician"].verdict.as_deref(), Some("PASS"));
        assert_eq!(by_title["Reviewer"].verdict, None);
        assert_eq!(by_title["Auditor"].verdict, None);
        assert_eq!(by_title["DoneAlready"].verdict, None);
        assert!(!by_title.contains_key("ExpiredRelay"));

        let w2 = waves.get("job-council").unwrap();
        assert_eq!(w2.plan, "council-r2");
        assert_eq!(w2.done, 1);
        assert_eq!(w2.relay_inflight, 0);
        assert_eq!(w2.pending, 2); // 3 Seat participants - 1 done = 2

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scans_auto_jobs_and_parses_camel_case_fields() {
        let d = std::env::temp_dir().join(format!("office-auto-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let j1 = d.join("auto-login-1");
        let j2 = d.join("auto-done-2");
        std::fs::create_dir_all(&j1).unwrap();
        std::fs::create_dir_all(&j2).unwrap();
        std::fs::write(
            j1.join("auto.json"),
            r#"{
                "job": "auto-login-1",
                "task": "Fix auth flow",
                "flow": "council",
                "phase": "execute",
                "status": "running",
                "callsUsed": 7,
                "maxCalls": 25,
                "startedAt": "2026-11-25T15:10:00Z",
                "elapsedBaseMs": 12000,
                "maxMinutes": 90,
                "driverPid": 4321,
                "abortRequested": false,
                "activeCalls": {
                    "worker-1": {"role": "worker", "pid": 5001},
                    "tester-1": {"role": "tester", "pid": 5002}
                },
                "arbitration": {"ruling": "APPROVE"},
                "branch": "agy/auto-login-1",
                "worktreePath": "/tmp/wt",
                "reportPath": "",
                "needsPath": "",
                "lastError": ""
            }"#,
        )
        .unwrap();
        std::fs::write(
            j2.join("auto.json"),
            r#"{
                "job": "auto-done-2",
                "task": "Refactor cache",
                "flow": "simple",
                "phase": "report",
                "status": "done",
                "callsUsed": 4,
                "maxCalls": 25,
                "startedAt": "bad-timestamp",
                "elapsedBaseMs": 45000,
                "maxMinutes": 90,
                "driverPid": null,
                "abortRequested": true,
                "activeCalls": {},
                "reportPath": "/tmp/r2/REPORT.md",
                "lastError": "boom"
            }"#,
        )
        .unwrap();

        let jobs = scan_auto_jobs(&d);
        assert_eq!(jobs.len(), 2);
        // Running job comes first
        assert_eq!(jobs[0].job, "auto-login-1");
        assert_eq!(jobs[0].task, "Fix auth flow");
        assert_eq!(jobs[0].flow, "council");
        assert_eq!(jobs[0].phase, "execute");
        assert_eq!(jobs[0].status, "running");
        assert_eq!((jobs[0].calls_used, jobs[0].max_calls), (7, 25));
        assert_eq!(jobs[0].started_at, parse_iso_ms("2026-11-25T15:10:00Z").unwrap());
        assert_eq!((jobs[0].elapsed_base_ms, jobs[0].max_minutes), (12_000, 90));
        assert_eq!(jobs[0].driver_pid, Some(4321));
        assert!(!jobs[0].abort_requested);
        assert_eq!(jobs[0].active_calls, 2);
        assert_eq!(jobs[0].ruling.as_deref(), Some("APPROVE"));
        assert_eq!(jobs[0].branch.as_deref(), Some("agy/auto-login-1"));
        assert_eq!(jobs[0].worktree_path.as_deref(), Some("/tmp/wt"));
        assert_eq!(jobs[0].report_path, None);
        assert_eq!(jobs[0].needs_path, None);
        assert_eq!(jobs[0].last_error, None);

        assert_eq!(jobs[1].job, "auto-done-2");
        assert_eq!(jobs[1].started_at, 0);
        assert_eq!(jobs[1].driver_pid, None);
        assert!(jobs[1].abort_requested);
        assert_eq!(jobs[1].active_calls, 0);
        assert_eq!(jobs[1].report_path.as_deref(), Some("/tmp/r2/REPORT.md"));
        assert_eq!(jobs[1].last_error.as_deref(), Some("boom"));

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn emits_auto_doc_reports_and_non_initial_alerts_without_duplicates() {
        let d = std::env::temp_dir().join(format!("office-auto-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let jdir = d.join("auto-doc-1");
        std::fs::create_dir_all(&jdir).unwrap();
        let report_file = jdir.join("REPORT.md");
        std::fs::write(&report_file, "# Final Report\nAll green.").unwrap();

        let job = AutoJob {
            job: "auto-doc-1".into(),
            task: "Ship feature".into(),
            flow: "council".into(),
            phase: "report".into(),
            status: "done".into(),
            ruling: Some("APPROVE".into()),
            report_path: Some(report_file.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let jobs = vec![job];
        let mut seen = HashMap::new();

        // Initial poll emits Boss Inbox Report Handoff, but NO Alert
        let evs1 = auto_doc_events(&d, &jobs, &mut seen, true);
        assert_eq!(evs1.len(), 1, "{evs1:?}");
        assert!(matches!(
            &evs1[0],
            EventKind::Handoff { from, to: None, kind: ReportKind::Report, title, body, brief }
            if from == "rosso-auto:auto-doc-1"
                && title == "rosso-auto auto-doc-1: REPORT (APPROVE)"
                && body.contains("All green.")
                && brief == "Ship feature"
        ));

        // Second poll with unchanged files emits nothing (no duplicate spam)
        assert!(auto_doc_events(&d, &jobs, &mut seen, false).is_empty());

        // New NEEDS_USER_INPUT.md written later -> emits Report Handoff + Warn Alert
        let needs_file = jdir.join("NEEDS_USER_INPUT.md");
        std::fs::write(&needs_file, "Security VETO: confirm override").unwrap();
        let evs3 = auto_doc_events(&d, &jobs, &mut seen, false);
        assert_eq!(evs3.len(), 2, "{evs3:?}");
        assert!(matches!(
            &evs3[0],
            EventKind::Handoff { from, to: None, kind: ReportKind::Report, title, body, .. }
            if from == "rosso-auto:auto-doc-1"
                && title == "rosso-auto auto-doc-1: NEEDS_USER_INPUT"
                && body.contains("Security VETO")
        ));
        assert!(matches!(
            &evs3[1],
            EventKind::Alert(Alert { level: AlertLevel::Warn, code, message, .. })
            if code == "rosso_needs_input" && message.contains("auto-doc-1")
        ));

        // Unchanged on next poll -> nothing
        assert!(auto_doc_events(&d, &jobs, &mut seen, false).is_empty());

        let _ = std::fs::remove_dir_all(&d);
    }
}
