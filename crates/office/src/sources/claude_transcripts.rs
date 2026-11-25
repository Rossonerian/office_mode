//! Tails ~/.claude/projects/**/*.jsonl for token usage -> Usage events (cost from config.prices, context %).
//!
//! Layout: `<project>/<session>.jsonl` (main session) and `<project>/<session>/subagents/agent-<id>.jsonl`
//! (+ `agent-<id>.meta.json` with agentType/finished). Live subagents get `cc-sub:<session>/<id>`;
//! finished or old ones (and sidechain lines without an agentId) roll up into the parent `cc:<session>`.
use super::basename;
use office_core::model::AgentKind;
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

const POLL: Duration = Duration::from_secs(2);
const DISCOVER_EVERY_POLLS: u64 = 2;
const WINDOW: Duration = Duration::from_secs(2 * 3600);
/// A subagent transcript untouched for longer than this rolls up into its parent.
const SUB_LIVE: Duration = Duration::from_secs(10 * 60);
const CONTEXT_TOKENS: f64 = 200_000.0;
const CONTEXT_TOKENS_1M: f64 = 1_000_000.0;
/// Max bytes read per chunk (up to 4 chunks per poll = 32 MB).
const CHUNK: u64 = 8 << 20;
const MAX_CHUNKS_PER_POLL: usize = 4;
const MAX_PARTIAL_LINE: usize = 16 << 20;
const MAX_RETIRED_OFFSETS: usize = 256;

#[derive(Deserialize)]
struct TranscriptLine<'a> {
    #[serde(borrow)]
    uuid: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(borrow)]
    message: Option<TranscriptMsg<'a>>,
}

#[derive(Deserialize)]
struct TranscriptMsg<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    usage: Option<TranscriptUsage>,
}

#[derive(Deserialize, Default)]
struct TranscriptUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Default, Clone, Copy, PartialEq)]
struct Tok {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

#[derive(Default)]
struct Totals {
    /// Fresh (uncached) input.
    tokens_in: u64,
    tokens_out: u64,
    tokens_cache_read: u64,
    tokens_cache_write: u64,
    cost: f64,
    context_pct: Option<f32>,
    model: Option<String>,
    /// message.id -> usage already counted (assistant messages are split over several lines).
    seen: HashMap<String, Tok>,
    dirty: bool,
}

struct FileState {
    offset: u64,
    partial: Vec<u8>,
    /// Agent id usage from this file is booked under.
    target: String,
    is_sub: bool,
    meta_mtime: Option<SystemTime>,
    meta_finished: bool,
}

pub struct Tailer {
    cfg: Config,
    root: PathBuf,
    files: HashMap<PathBuf, FileState>,
    totals: HashMap<String, Totals>,
    announced: HashSet<String>,
    /// main session id -> last seen modification (for Gone when it leaves the 2h window).
    mains: HashMap<String, SystemTime>,
    /// Offset preserved for expired main sessions so re-opening doesn't double-count retired tokens.
    retired_offsets: HashMap<PathBuf, u64>,
    retired_order: VecDeque<PathBuf>,
    poll_count: u64,
}

pub fn spawn(cfg: &Config, tx: Sender<Event>) -> anyhow::Result<()> {
    let mut t = Tailer::new(cfg.clone(), paths::claude_dir().join("projects"));
    std::thread::Builder::new().name("claude-transcripts".into()).spawn(move || loop {
        for e in t.poll(SystemTime::now()) {
            if tx.send(e).is_err() {
                return;
            }
        }
        std::thread::sleep(POLL);
    })?;
    Ok(())
}

fn ev(kind: EventKind) -> Event {
    Event::now(SourceKind::ClaudeTranscript, kind)
}

impl Tailer {
    pub fn new(cfg: Config, root: PathBuf) -> Self {
        Tailer {
            cfg,
            root,
            files: HashMap::new(),
            totals: HashMap::new(),
            announced: HashSet::new(),
            mains: HashMap::new(),
            retired_offsets: HashMap::new(),
            retired_order: VecDeque::new(),
            poll_count: 0,
        }
    }

    fn remember_retired_offset(&mut self, path: PathBuf, offset: u64) {
        if offset == 0 {
            return;
        }
        if !self.retired_offsets.contains_key(&path) {
            self.retired_order.push_back(path.clone());
            while self.retired_order.len() > MAX_RETIRED_OFFSETS {
                if let Some(old) = self.retired_order.pop_front() {
                    self.retired_offsets.remove(&old);
                }
            }
        }
        self.retired_offsets.insert(path, offset);
    }

    /// One scan: find recent transcripts, read new bytes, return AgentSeen/Usage/Gone events.
    pub fn poll(&mut self, now: SystemTime) -> Vec<Event> {
        let mut out = Vec::new();
        let mut found: Vec<(PathBuf, SystemTime)> = Vec::new();
        let full_scan = self.poll_count % DISCOVER_EVERY_POLLS == 0;
        self.poll_count = self.poll_count.wrapping_add(1);
        if full_scan {
            collect(&self.root, 0, &mut found);
        } else {
            let tracked: Vec<PathBuf> = self.files.keys().cloned().collect();
            for p in tracked {
                match std::fs::metadata(&p) {
                    Ok(m) => {
                        if let Ok(mt) = m.modified() {
                            found.push((p, mt));
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        self.files.remove(&p);
                    }
                    Err(_) => {}
                }
            }
        }

        let mut finished_subs: Vec<(PathBuf, String)> = Vec::new();
        for (path, mtime) in found {
            let age = now.duration_since(mtime).unwrap_or_default();
            if age > WINDOW && !self.files.contains_key(&path) {
                continue;
            }
            if !self.files.contains_key(&path) {
                let Some(st) = self.first_sight(&path, age, &mut out) else { continue };
                self.files.insert(path.clone(), st);
            }
            if let Some(sess) = main_session(&path) {
                self.mains.entry(sess).and_modify(|m| *m = (*m).max(mtime)).or_insert(mtime);
            } else if let Some((sess, _)) = sub_ids(&path) {
                if let Some(m) = self.mains.get_mut(&sess) {
                    *m = (*m).max(mtime);
                }
            }
            self.read_new(&path);
            // A live sub whose meta now says finished: queue Gone after dirty Usage is emitted.
            if let Some(st) = self.files.get_mut(&path) {
                if st.is_sub && self.announced.contains(&st.target) && cached_sub_finished(&path, st) {
                    finished_subs.push((path.clone(), st.target.clone()));
                }
            }
        }

        // 1. Always emit dirty Usage BEFORE any Gone events so reducers never resurrect finished agents.
        for (id, t) in self.totals.iter_mut() {
            if t.dirty {
                t.dirty = false;
                out.push(ev(EventKind::Usage {
                    id: id.clone(),
                    tokens_in: t.tokens_in,
                    tokens_out: t.tokens_out,
                    tokens_cache_read: t.tokens_cache_read,
                    tokens_cache_write: t.tokens_cache_write,
                    cost_usd: t.cost,
                    context_pct: t.context_pct,
                    model: t.model.clone(),
                }));
            }
        }

        // 2. Emit Gone for finished subagents and purge their state (remembering offset so re-discovery doesn't double-count).
        for (path, target) in finished_subs {
            if self.announced.remove(&target) {
                out.push(ev(EventKind::Gone { id: target.clone() }));
            }
            self.totals.remove(&target);
            if let Some(st) = self.files.remove(&path) {
                self.remember_retired_offset(path, st.offset);
            }
        }

        // 3. Sessions that dropped out of the 2h window leave the office; preserve offset to avoid double-counting on resume.
        let expired: Vec<String> =
            self.mains.iter().filter(|(_, m)| now.duration_since(**m).unwrap_or_default() > WINDOW).map(|(s, _)| s.clone()).collect();
        for s in expired {
            self.mains.remove(&s);
            let id = format!("cc:{s}");
            if self.announced.remove(&id) {
                out.push(ev(EventKind::Gone { id: id.clone() }));
            }
            self.totals.remove(&id);
            let paths: Vec<PathBuf> = self
                .files
                .keys()
                .filter(|p| main_session(p).as_deref() == Some(s.as_str()) || sub_ids(p).is_some_and(|(ps, _)| ps == s))
                .cloned()
                .collect();
            for p in paths {
                if let Some(st) = self.files.remove(&p) {
                    self.totals.remove(&st.target);
                    self.announced.remove(&st.target);
                    self.remember_retired_offset(p, st.offset);
                }
            }
        }
        out
    }

    fn first_sight(&mut self, path: &Path, age: Duration, out: &mut Vec<Event>) -> Option<FileState> {
        let start_offset = self.retired_offsets.remove(path).unwrap_or(0);
        if let Some(sess) = main_session(path) {
            let id = format!("cc:{sess}");
            if self.announced.insert(id.clone()) {
                let cwd = first_cwd(path);
                out.push(ev(EventKind::AgentSeen {
                    id: id.clone(),
                    kind: AgentKind::Claude,
                    label: cwd.as_deref().map(|c| format!("cc·{}", basename(c))),
                    parent: None,
                    cwd,
                    role: None,
                    model: None,
                    slot: None,
                    pid: None,
                    resume_id: Some(sess),
                }));
            }
            return Some(FileState {
                offset: start_offset,
                partial: Vec::new(),
                target: id,
                is_sub: false,
                meta_mtime: None,
                meta_finished: false,
            });
        }
        let (sess, agent) = sub_ids(path)?;
        let parent = format!("cc:{sess}");
        let (meta_mtime, finished, meta) = inspect_meta(path);
        let live = age <= SUB_LIVE && !finished;
        if !live {
            return Some(FileState {
                offset: start_offset,
                partial: Vec::new(),
                target: parent,
                is_sub: false,
                meta_mtime,
                meta_finished: finished,
            });
        }
        let id = format!("cc-sub:{sess}/{agent}");
        let role = meta.as_ref().and_then(|m| m.get("agentType")).and_then(Value::as_str).map(str::to_string);
        out.push(ev(EventKind::AgentSeen {
            id: id.clone(),
            kind: AgentKind::ClaudeSub,
            label: Some(role.clone().unwrap_or_else(|| "sub".into())),
            parent: Some(parent),
            cwd: None,
            role,
            model: meta.as_ref().and_then(|m| m.get("model")).and_then(Value::as_str).map(str::to_string),
            slot: None,
            pid: None,
            resume_id: None,
        }));
        self.announced.insert(id.clone());
        Some(FileState {
            offset: start_offset,
            partial: Vec::new(),
            target: id,
            is_sub: true,
            meta_mtime,
            meta_finished: false,
        })
    }

    fn read_new(&mut self, path: &Path) {
        let Some(st) = self.files.get_mut(path) else { return };
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                self.files.remove(path);
                return;
            }
            Err(_) => return,
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < st.offset {
            // truncated/rewritten: start over (message-id dedupe prevents double counting)
            st.offset = 0;
            st.partial.clear();
        }
        if len == st.offset {
            return;
        }
        let mut f = f;
        if f.seek(SeekFrom::Start(st.offset)).is_err() {
            return;
        }
        let target = st.target.clone();
        let is_sub = st.is_sub;
        let mut total_read = 0u64;
        for _ in 0..MAX_CHUNKS_PER_POLL {
            let mut buf = Vec::new();
            let n = (&mut f).take(CHUNK).read_to_end(&mut buf).unwrap_or(0) as u64;
            if n == 0 {
                break;
            }
            total_read += n;
            let (new_off, complete) = {
                let st = self.files.get_mut(path).expect("present");
                st.offset += n;
                st.partial.extend_from_slice(&buf);
                let complete = if let Some(last_nl) = st.partial.iter().rposition(|b| *b == b'\n') {
                    Some(st.partial.drain(..=last_nl).collect::<Vec<u8>>())
                } else {
                    if st.partial.len() > MAX_PARTIAL_LINE {
                        st.partial.clear();
                    }
                    None
                };
                (st.offset, complete)
            };
            if let Some(complete) = complete {
                for line in complete.split(|b| *b == b'\n') {
                    if line.is_empty() || !line.windows(7).any(|w| w == b"\"usage\"") {
                        continue;
                    }
                    let Ok(v) = serde_json::from_slice::<TranscriptLine<'_>>(line) else { continue };
                    self.ingest(&target, is_sub, &v);
                }
            }
            if new_off >= len || n < CHUNK {
                break;
            }
        }
        let _ = total_read;
    }

    fn ingest(&mut self, target: &str, _is_sub: bool, v: &TranscriptLine<'_>) {
        let Some(msg) = &v.message else { return };
        let Some(usage) = &msg.usage else { return };
        let tok = Tok {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_write: usage.cache_creation_input_tokens,
            cache_read: usage.cache_read_input_tokens,
        };
        let model = msg.model.as_deref().unwrap_or("");
        if model == "<synthetic>" {
            return;
        }
        let key = msg
            .id
            .as_deref()
            .map(str::to_string)
            .or_else(|| v.uuid.as_deref().map(str::to_string))
            .unwrap_or_default();
        let t = self.totals.entry(target.to_string()).or_default();
        let prev = t.seen.get(&key).copied().unwrap_or_default();
        if prev == tok {
            return;
        }
        // Book only the growth over what this message already contributed.
        let d = Tok {
            input: tok.input.saturating_sub(prev.input),
            output: tok.output.saturating_sub(prev.output),
            cache_write: tok.cache_write.saturating_sub(prev.cache_write),
            cache_read: tok.cache_read.saturating_sub(prev.cache_read),
        };
        if !key.is_empty() {
            let merged = Tok {
                input: prev.input.max(tok.input),
                output: prev.output.max(tok.output),
                cache_write: prev.cache_write.max(tok.cache_write),
                cache_read: prev.cache_read.max(tok.cache_read),
            };
            t.seen.insert(key, merged);
        }
        t.tokens_in += d.input;
        t.tokens_out += d.output;
        t.tokens_cache_read += d.cache_read;
        t.tokens_cache_write += d.cache_write;
        t.cost += self.cfg.cost_usd(model, d.input, d.output, d.cache_write, d.cache_read);
        if !v.is_sidechain && !model.is_empty() {
            t.model = Some(model.to_string());
        }
        // Context % reflects the agent's own window; rolled-up sidechain lines don't overwrite the parent's.
        if !v.is_sidechain || target.starts_with("cc-sub:") {
            let ctx = (tok.input + tok.cache_write + tok.cache_read) as f64;
            let limit = context_window_for(model);
            t.context_pct = Some(((ctx / limit) * 100.0).min(100.0) as f32);
        }
        t.dirty = true;
    }
}

fn context_window_for(model: &str) -> f64 {
    let m = model.to_ascii_lowercase();
    if m.contains("1m") || m.contains("opus-4-6") || m.contains("sonnet-4-6") {
        CONTEXT_TOKENS_1M
    } else {
        CONTEXT_TOKENS
    }
}

/// Recursively collect *.jsonl under root (depth <= 3: project/session/subagents/file).
fn collect(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < 3 {
                collect(&p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                out.push((p, m));
            }
        }
    }
}

/// `<project>/<session>.jsonl` -> session id.
fn main_session(p: &Path) -> Option<String> {
    let parent_name = p.parent()?.file_name()?.to_str()?;
    if parent_name == "subagents" {
        return None;
    }
    let stem = p.file_stem()?.to_str()?;
    if stem.starts_with("agent-") {
        return None;
    }
    Some(stem.to_string())
}

/// `<project>/<session>/subagents/agent-<id>.jsonl` -> (session, id).
fn sub_ids(p: &Path) -> Option<(String, String)> {
    let parent = p.parent()?;
    if parent.file_name()?.to_str()? != "subagents" {
        return None;
    }
    let sess = parent.parent()?.file_name()?.to_str()?.to_string();
    let agent = p.file_stem()?.to_str()?.strip_prefix("agent-")?.to_string();
    Some((sess, agent))
}

fn inspect_meta(p: &Path) -> (Option<SystemTime>, bool, Option<Value>) {
    let mpath = p.with_extension("meta.json");
    let mtime = std::fs::metadata(&mpath).and_then(|m| m.modified()).ok();
    let val = std::fs::read_to_string(&mpath).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let finished = val.as_ref().and_then(|m| m.get("finished").and_then(Value::as_bool)).unwrap_or(false);
    (mtime, finished, val)
}

fn cached_sub_finished(p: &Path, st: &mut FileState) -> bool {
    let mpath = p.with_extension("meta.json");
    let mtime = std::fs::metadata(&mpath).and_then(|m| m.modified()).ok();
    if mtime == st.meta_mtime {
        return st.meta_finished;
    }
    st.meta_mtime = mtime;
    st.meta_finished = std::fs::read_to_string(&mpath)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|m| m.get("finished").and_then(Value::as_bool))
        .unwrap_or(false);
    st.meta_finished
}

/// cwd from the first 256 KB of a transcript (streaming line by line).
fn first_cwd(p: &Path) -> Option<String> {
    let f = std::fs::File::open(p).ok()?;
    let mut r = BufReader::new(f.take(256 * 1024));
    let mut line = String::new();
    while r.read_line(&mut line).ok()? > 0 {
        if line.contains("\"cwd\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    return Some(c.to_string());
                }
            }
        }
        line.clear();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(id: &str, model: &str, i: u64, o: u64, cw: u64, cr: u64) -> String {
        format!(
            r#"{{"cwd":"/w/proj","type":"assistant","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{i},"output_tokens":{o},"cache_creation_input_tokens":{cw},"cache_read_input_tokens":{cr}}}}}}}"#
        ) + "\n"
    }

    #[test]
    fn sums_usage_dedupes_and_rolls_up() {
        let root = std::env::temp_dir().join(format!("office-tr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("-w-proj");
        std::fs::create_dir_all(proj.join("s1/subagents")).unwrap();
        let main = proj.join("s1.jsonl");
        let mut f = std::fs::File::create(&main).unwrap();
        f.write_all(line("m1", "claude-opus-5", 1000, 100, 0, 0).as_bytes()).unwrap();
        f.write_all(line("m1", "claude-opus-5", 1000, 100, 0, 0).as_bytes()).unwrap(); // same message, 2nd block
        // finished subagent -> rolls into parent
        std::fs::write(proj.join("s1/subagents/agent-ab.jsonl"), line("m2", "claude-haiku-5", 10, 10, 0, 0)).unwrap();
        std::fs::write(proj.join("s1/subagents/agent-ab.meta.json"), r#"{"agentType":"Explore","finished":true}"#).unwrap();
        // live subagent -> own id
        std::fs::write(proj.join("s1/subagents/agent-cd.jsonl"), line("m3", "claude-haiku-5", 20, 0, 0, 0)).unwrap();
        std::fs::write(proj.join("s1/subagents/agent-cd.meta.json"), r#"{"agentType":"Plan","finished":false}"#).unwrap();

        let mut t = Tailer::new(Config::default(), root.clone());
        let evs = t.poll(SystemTime::now());
        let seen: Vec<String> =
            evs.iter().filter_map(|e| if let EventKind::AgentSeen { id, .. } = &e.kind { Some(id.clone()) } else { None }).collect();
        assert!(seen.contains(&"cc:s1".to_string()));
        assert!(seen.contains(&"cc-sub:s1/cd".to_string()));
        assert!(!seen.iter().any(|s| s.ends_with("/ab")));
        let usage = |evs: &[Event], who: &str| {
            evs.iter().find_map(|e| match &e.kind {
                EventKind::Usage { id, tokens_in, tokens_out, cost_usd, .. } if id == who => Some((*tokens_in, *tokens_out, *cost_usd)),
                _ => None,
            })
        };
        let cache = |evs: &[Event], who: &str| {
            evs.iter().find_map(|e| match &e.kind {
                EventKind::Usage { id, tokens_cache_read, tokens_cache_write, .. } if id == who => Some((*tokens_cache_read, *tokens_cache_write)),
                _ => None,
            })
        };
        let (i, o, c) = usage(&evs, "cc:s1").unwrap();
        assert_eq!((i, o), (1010, 110));
        let expect = (1000.0 * 15.0 + 100.0 * 75.0 + 10.0 * 0.8 + 10.0 * 4.0) / 1e6;
        assert!((c - expect).abs() < 1e-9, "{c} vs {expect}");
        assert_eq!(usage(&evs, "cc-sub:s1/cd").unwrap().0, 20);

        // tail: append a new message incl. a partial line
        let mut f = std::fs::OpenOptions::new().append(true).open(&main).unwrap();
        let l = line("m4", "claude-opus-5", 0, 0, 1000, 10_000);
        let (a, b) = l.split_at(20);
        f.write_all(a.as_bytes()).unwrap();
        assert!(usage(&t.poll(SystemTime::now()), "cc:s1").is_none());
        f.write_all(b.as_bytes()).unwrap();
        let evs = t.poll(SystemTime::now());
        let (i, _, c2) = usage(&evs, "cc:s1").unwrap();
        // cache tokens are booked separately (fresh input unchanged); cost still counts them
        assert_eq!(i, 1010);
        assert_eq!(cache(&evs, "cc:s1"), Some((10_000, 1000)));
        assert!(c2 > c);
        let ctx = evs.iter().find_map(|e| match &e.kind {
            EventKind::Usage { id, context_pct, .. } if id == "cc:s1" => *context_pct,
            _ => None,
        });
        assert!((ctx.unwrap() - 5.5).abs() < 0.01);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn usage_emitted_before_gone_and_resume_avoids_double_count() {
        let root = std::env::temp_dir().join(format!("office-tr-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("-w-proj");
        std::fs::create_dir_all(proj.join("s1/subagents")).unwrap();
        let main = proj.join("s1.jsonl");
        std::fs::write(&main, line("m1", "claude-opus-5", 100, 10, 0, 0)).unwrap();
        let sub = proj.join("s1/subagents/agent-xy.jsonl");
        let meta = proj.join("s1/subagents/agent-xy.meta.json");
        std::fs::write(&sub, line("sm1", "claude-haiku-5", 50, 5, 0, 0)).unwrap();
        std::fs::write(&meta, r#"{"agentType":"Explore","finished":false}"#).unwrap();

        let t0 = SystemTime::now();
        let mut t = Tailer::new(Config::default(), root.clone());
        let _ = t.poll(t0);

        // Sub finishes with trailing usage in the same poll: Usage MUST precede Gone.
        let mut sf = std::fs::OpenOptions::new().append(true).open(&sub).unwrap();
        sf.write_all(line("sm2", "claude-haiku-5", 25, 0, 0, 0).as_bytes()).unwrap();
        std::fs::write(&meta, r#"{"agentType":"Explore","finished":true}"#).unwrap();
        let evs = t.poll(t0 + Duration::from_secs(2));
        let u_pos = evs.iter().position(|e| matches!(&e.kind, EventKind::Usage { id, tokens_in: 75, .. } if id == "cc-sub:s1/xy")).unwrap();
        let g_pos = evs.iter().position(|e| matches!(&e.kind, EventKind::Gone { id } if id == "cc-sub:s1/xy")).unwrap();
        assert!(u_pos < g_pos, "Usage must precede Gone");

        // Main expires past 2h window, then resumes with new message -> no double-counting of m1.
        let exp = t.poll(t0 + WINDOW + Duration::from_secs(10));
        assert!(exp.iter().any(|e| matches!(&e.kind, EventKind::Gone { id } if id == "cc:s1")));

        let mut mf = std::fs::OpenOptions::new().append(true).open(&main).unwrap();
        mf.write_all(line("m2", "claude-opus-5", 200, 20, 0, 0).as_bytes()).unwrap();
        t.poll_count = 0; // trigger discovery scan
        let res = t.poll(SystemTime::now());
        assert!(res.iter().any(|e| matches!(&e.kind, EventKind::AgentSeen { id, .. } if id == "cc:s1")));
        let tin = res.iter().find_map(|e| match &e.kind {
            EventKind::Usage { id, tokens_in, .. } if id == "cc:s1" => Some(*tokens_in),
            _ => None,
        });
        assert_eq!(tin, Some(200));
        let _ = std::fs::remove_dir_all(&root);
    }
}
