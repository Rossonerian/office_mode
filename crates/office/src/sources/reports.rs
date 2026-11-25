//! Pull hand-off documents out of Claude Code transcripts: a subagent's brief (its first user
//! message) and its final report (its hand-back, or its last assistant text), and a session's
//! last answer. Bounded reads only; every function returns None rather than failing.
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const TAIL_BYTES: u64 = 768 * 1024;
const HEAD_BYTES: u64 = 256 * 1024;

/// `<project>/<session>.jsonl` + agent id -> `<project>/<session>/subagents/agent-<id>.jsonl`.
pub fn subagent_transcript(session_transcript: &Path, agent_id: &str) -> PathBuf {
    let stem = session_transcript.file_stem().map(|s| s.to_os_string()).unwrap_or_default();
    let dir = session_transcript.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(stem).join("subagents").join(format!("agent-{agent_id}.jsonl"))
}

/// The `description` from the subagent's `.meta.json` (e.g. "Backend work order").
pub fn subagent_description(agent_transcript: &Path) -> Option<String> {
    let meta = agent_transcript.with_extension("meta.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    v.get("description").and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty())
}

fn read_range(path: &Path, from_end: bool, n: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = if from_end { len.saturating_sub(n) } else { 0 };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Text of a message's content: a string, or its text blocks joined.
fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let t: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            (!t.is_empty()).then(|| t.join("\n"))
        }
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
}

/// A hand-back tool call (`SubagentHandback { message }` and similar) inside an assistant message.
fn handback_text(content: &Value) -> Option<String> {
    content.as_array()?.iter().rev().find_map(|b| {
        if b.get("type").and_then(Value::as_str) != Some("tool_use") {
            return None;
        }
        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
        if !name.to_ascii_lowercase().contains("handback") {
            return None;
        }
        b.get("input").and_then(|i| i.get("message")).and_then(Value::as_str).map(str::to_string)
    })
}

/// The final report in a transcript: the last hand-back message, else the last assistant text.
pub fn last_report(path: &Path) -> Option<String> {
    let text = read_range(path, true, TAIL_BYTES)?;
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(msg) = v.get("message") else { continue };
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let content = msg.get("content").unwrap_or(&Value::Null);
        if let Some(t) = handback_text(content).or_else(|| content_text(content)) {
            return Some(t);
        }
    }
    None
}

/// The brief a subagent was given: its first user message.
pub fn first_brief(path: &Path) -> Option<String> {
    let text = read_range(path, false, HEAD_BYTES)?;
    text.lines().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        let msg = v.get("message")?;
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        content_text(msg.get("content")?)
    })
}

/// First non-empty line of `s`, cut to `n` chars: a document title.
pub fn title_of(s: &str, n: usize) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '*', '-', ' ']);
    super::short(line, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_brief_handback_and_description() {
        let dir = std::env::temp_dir().join(format!("office-reports-{}", std::process::id()));
        let session = dir.join("s1.jsonl");
        let agent = subagent_transcript(&session, "a9");
        std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
        let lines = [
            r#"{"message":{"role":"user","content":"--domain backend. Produce a work order"}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"#,
            r#"{"message":{"role":"assistant","content":[{"type":"thinking"},{"type":"tool_use","name":"SubagentHandback","input":{"message":"STATUS: OK\nWork order ready"}}]}}"#,
            r#"{"message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
        ];
        std::fs::write(&agent, lines.join("\n")).unwrap();
        std::fs::write(agent.with_extension("meta.json"), r#"{"agentType":"agy-manager","description":"Backend work order"}"#).unwrap();
        assert_eq!(first_brief(&agent).as_deref(), Some("--domain backend. Produce a work order"));
        assert_eq!(last_report(&agent).as_deref(), Some("STATUS: OK\nWork order ready"));
        assert_eq!(subagent_description(&agent).as_deref(), Some("Backend work order"));
        assert_eq!(title_of("\n## Plan: fix it\nmore", 40), "Plan: fix it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
