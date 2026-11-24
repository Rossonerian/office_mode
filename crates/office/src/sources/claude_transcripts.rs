//! Tails ~/.claude/projects/**/*.jsonl for token usage -> Usage events (cost from config.prices, context %).
//!
//! Layout: `<project>/<session>.jsonl` (main session) and `<project>/<session>/subagents/agent-<id>.jsonl`
//! (+ `agent-<id>.meta.json` with agentType/finished). Live subagents get `cc-sub:<session>/<id>`;
//! finished or old ones (and sidechain lines without an agentId) roll up into the parent `cc:<session>`.
use super::basename;
use office_core::model::AgentKind;
use office_core::{config::Config, paths, Event, EventKind, SourceKind};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

const POLL: Duration = Duration::from_secs(2);
const WINDOW: Duration = Duration::from_secs(2 * 3600);
/// A subagent transcript untouched for longer than this rolls up into its parent.
const SUB_LIVE: Duration = Duration::from_secs(10 * 60);
const CONTEXT_TOKENS: f64 = 200_000.0;
/// Max bytes read per file per poll (keeps first sight of huge transcripts bounded per tick).
const CHUNK: u64 = 8 << 20;

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
}

pub struct Tailer {
    cfg: Config,
    root: PathBuf,
    files: HashMap<PathBuf, FileState>,
    totals: HashMap<String, Totals>,
    announced: HashSet<String>,
    /// main session id -> last seen modification (for Gone when it leaves the 2h window).
    mains: HashMap<String, SystemTime>,
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

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

impl Tailer {
    pub fn new(cfg: Config, root: PathBuf) -> Self {
        Tailer { cfg, root, files: HashMap::new(), totals: HashMap::new(), announced: HashSet::new(), mains: HashMap::new() }
    }

    /// One scan: find recent transcripts, read new bytes, return AgentSeen/Usage/Gone events.
    pub fn poll(&mut self, now: SystemTime) -> Vec<Event> {
        let mut out = Vec::new();
        let mut found = Vec::new();
        collect(&self.root, 0, &mut found);
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
                self.mains.insert(sess, mtime);
            }
            self.read_new(&path);
            // A live sub whose meta now says finished: let it leave.
            let st = &self.files[&path];
            if st.is_sub && self.announced.contains(&st.target) && sub_finished(&path) {
                out.push(ev(EventKind::Gone { id: st.target.clone() }));
                self.announced.remove(&st.target.clone());
            }
        }
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
                }));
            }
        }
        // Sessions that dropped out of the 2h window leave the office.
        let expired: Vec<String> =
            self.mains.iter().filter(|(_, m)| now.duration_since(**m).unwrap_or_default() > WINDOW).map(|(s, _)| s.clone()).collect();
        for s in expired {
            self.mains.remove(&s);
            let id = format!("cc:{s}");
            if self.announced.remove(&id) {
                out.push(ev(EventKind::Gone { id }));
            }
        }
        out
    }

    fn first_sight(&mut self, path: &Path, age: Duration, out: &mut Vec<Event>) -> Option<FileState> {
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
            return Some(FileState { offset: 0, partial: Vec::new(), target: id, is_sub: false });
        }
        let (sess, agent) = sub_ids(path)?;
        let parent = format!("cc:{sess}");
        let live = age <= SUB_LIVE && !sub_finished(path);
        if !live {
            return Some(FileState { offset: 0, partial: Vec::new(), target: parent, is_sub: false });
        }
        let id = format!("cc-sub:{sess}/{agent}");
        let meta = read_meta(path);
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
        Some(FileState { offset: 0, partial: Vec::new(), target: id, is_sub: true })
    }

    fn read_new(&mut self, path: &Path) {
        let Some(st) = self.files.get_mut(path) else { return };
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < st.offset {
            // truncated/rewritten: start over (message-id dedupe prevents double counting)
            st.offset = 0;
            st.partial.clear();
        }
        if len == st.offset || f.seek(SeekFrom::Start(st.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        let n = f.take(CHUNK).read_to_end(&mut buf).unwrap_or(0) as u64;
        st.offset += n;
        st.partial.extend_from_slice(&buf);
        let Some(last_nl) = st.partial.iter().rposition(|b| *b == b'\n') else { return };
        let complete: Vec<u8> = st.partial.drain(..=last_nl).collect();
        let target = st.target.clone();
        let is_sub = st.is_sub;
        for line in complete.split(|b| *b == b'\n') {
            if line.is_empty() || !line.windows(7).any(|w| w == b"\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            self.ingest(&target, is_sub, &v);
        }
    }

    fn ingest(&mut self, target: &str, _is_sub: bool, v: &Value) {
        let Some(msg) = v.get("message") else { return };
        let Some(usage) = msg.get("usage") else { return };
        let tok = Tok {
            input: u(usage, "input_tokens"),
            output: u(usage, "output_tokens"),
            cache_write: u(usage, "cache_creation_input_tokens"),
            cache_read: u(usage, "cache_read_input_tokens"),
        };
        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if model == "<synthetic>" {
            return;
        }
        let key = msg
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.get("uuid").and_then(Value::as_str).map(str::to_string))
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
        // Context % reflects the agent's own window; rolled-up sidechain lines don't overwrite the parent's.
        let sidechain = v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
        if !sidechain || target.starts_with("cc-sub:") {
            let ctx = (tok.input + tok.cache_write + tok.cache_read) as f64;
            t.context_pct = Some(((ctx / CONTEXT_TOKENS) * 100.0).min(100.0) as f32);
        }
        t.dirty = true;
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

fn read_meta(p: &Path) -> Option<Value> {
    let m = p.with_extension("meta.json");
    serde_json::from_str(&std::fs::read_to_string(m).ok()?).ok()
}

fn sub_finished(p: &Path) -> bool {
    read_meta(p).and_then(|m| m.get("finished").and_then(Value::as_bool)).unwrap_or(false)
}

/// cwd from the first lines of a transcript.
fn first_cwd(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    for line in buf.split(|b| *b == b'\n') {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                return Some(c.to_string());
            }
        }
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
}
