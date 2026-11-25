//! The report reader: turns an agent's raw hand-off text into something friendly to read.
//! - collapses the agy router header (STATUS / MODEL / ROLE / ... / RESPONSE:) into one status line,
//! - renders light Markdown (headings, bullets, numbered lists, **bold**, `code`, fences, tables),
//! - wraps to a fixed column width itself, so scrolling can count the rendered lines exactly.
use super::fmt::{fmt_ago, kind_color, trunc};
use office_core::{Report, ReportKind, Scene};
use ratatui::{
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
};

/// Maximum text width of the reader (columns), for comfortable line lengths.
pub const MAX_WIDTH: usize = 96;

const HEADER_KEYS: [&str; 17] = [
    "STATUS", "ACCOUNT", "MODEL", "ROLE", "QUEUED_MS", "SLOT", "CAPACITY", "FREE_MB", "PER_PROC_MB", "RESERVED_MB",
    "PRESSURE", "DURATION_MS", "ERROR_CODE", "ERROR", "NEEDS_USER_INPUT", "RESPONSE_FILE", "SESSION",
];

/// A report body split into the router's header fields and the actual content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Router header fields in order (empty when the body has no router block).
    pub fields: Vec<(String, String)>,
    /// Everything after `RESPONSE:` (or the whole body when there is no router block).
    pub content: String,
}

fn header_field(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(':')?;
    let k = k.trim();
    if k == "RESPONSE" || HEADER_KEYS.contains(&k) {
        Some((k, v.trim()))
    } else {
        None
    }
}

/// Split an agy router block off the front of a report body.
pub fn parse_body(body: &str) -> Parsed {
    let mut lines = body.lines().peekable();
    // Skip leading blanks without consuming real content.
    while lines.peek().is_some_and(|l| l.trim().is_empty()) {
        lines.next();
    }
    let first_is_header = lines.peek().and_then(|l| header_field(l)).is_some_and(|(k, _)| k != "RESPONSE");
    if !first_is_header {
        return Parsed { fields: Vec::new(), content: body.trim().to_string() };
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut content = String::new();
    while let Some(line) = lines.next() {
        match header_field(line) {
            Some(("RESPONSE", rest)) => {
                content.push_str(rest);
                for l in lines.by_ref() {
                    content.push('\n');
                    content.push_str(l);
                }
                break;
            }
            Some((k, v)) => fields.push((k.to_string(), v.to_string())),
            None => {
                // A continuation of a multi-line field (error text, a question), else content without a marker.
                let multi = fields.last().is_some_and(|(k, _)| matches!(k.as_str(), "NEEDS_USER_INPUT" | "ERROR"));
                if multi || line.trim().is_empty() && content.is_empty() {
                    if multi && !line.trim().is_empty() {
                        if let Some(last) = fields.last_mut() {
                            last.1.push(' ');
                            last.1.push_str(line.trim());
                        }
                    }
                } else {
                    content.push_str(line);
                    for l in lines.by_ref() {
                        content.push('\n');
                        content.push_str(l);
                    }
                    break;
                }
            }
        }
    }
    Parsed { fields, content: content.trim().to_string() }
}

fn field<'a>(p: &'a Parsed, key: &str) -> Option<&'a str> {
    p.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
}

/// "3 m 58 s" / "42 s" / "1 h 02 m".
pub fn human_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} m {:02} s", s / 60, s % 60)
    } else {
        format!("{} h {:02} m", s / 3600, (s % 3600) / 60)
    }
}

/// Friendly job title for an agy router role.
pub fn role_title(role: &str) -> String {
    match role {
        "architect" => "Design Engineer",
        "worker" => "Technician",
        "supervisor" => "Chief Inspector",
        "tester" => "Test Inspector",
        "git" => "Records Clerk",
        "manager" => "Department Head",
        "council-architect" | "council_architect" => "Chief Architect",
        "council-advisor" | "council_advisor" => "Review Panel",
        other => return other.to_string(),
    }
    .to_string()
}

/// One friendly line for the router header, e.g. "✓ OK · Chief Architect (neon-xhigh) · 3 m 58 s".
/// None when the body had no router block.
pub fn status_line(p: &Parsed) -> Option<(String, Color)> {
    let status = field(p, "STATUS")?;
    let ok = status.eq_ignore_ascii_case("ok");
    let blocked = status.eq_ignore_ascii_case("blocked") || field(p, "NEEDS_USER_INPUT").is_some();
    let mut parts: Vec<String> = Vec::new();
    parts.push(if ok {
        "✓ OK".to_string()
    } else if blocked {
        "? Needs your answer".to_string()
    } else {
        format!("✗ {}", status)
    });
    if let Some(code) = field(p, "ERROR_CODE") {
        parts.push(code.to_string());
    }
    let who = match (field(p, "ROLE"), field(p, "MODEL")) {
        (Some(r), Some(m)) => Some(format!("{} ({})", role_title(r), m.strip_prefix("gemini-4-").unwrap_or(m))),
        (Some(r), None) => Some(role_title(r)),
        (None, Some(m)) => Some(m.strip_prefix("gemini-4-").unwrap_or(m).to_string()),
        _ => None,
    };
    parts.extend(who);
    if let Some(ms) = field(p, "DURATION_MS").and_then(|v| v.parse::<u64>().ok()) {
        parts.push(human_duration(ms));
    }
    if let Some(q) = field(p, "QUEUED_MS").and_then(|v| v.parse::<u64>().ok()).filter(|q| *q >= 5_000) {
        parts.push(format!("waited {}", human_duration(q)));
    }
    let color = if ok {
        Color::Green
    } else if blocked {
        Color::Yellow
    } else {
        Color::LightRed
    };
    Some((parts.join(" · "), color))
}

/// Extra prominent lines for errors / questions the router raised (empty for a clean OK).
pub fn alert_lines(p: &Parsed) -> Vec<(String, Color)> {
    let mut v = Vec::new();
    if let Some(e) = field(p, "ERROR") {
        v.push((format!("Error: {e}"), Color::LightRed));
    }
    if let Some(q) = field(p, "NEEDS_USER_INPUT") {
        v.push((format!("Needs your input: {q}"), Color::Yellow));
    }
    v
}

// ---------- inline markdown ----------

const CODE_FG: Color = Color::Rgb(0x6f, 0xb5, 0xc4);

fn inline(text: &str, base: Style) -> Vec<(char, Style)> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(cs.len());
    let mut bold = false;
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '*' && cs.get(i + 1) == Some(&'*') {
            bold = !bold;
            i += 2;
        } else if cs[i] == '`' {
            match cs[i + 1..].iter().position(|c| *c == '`') {
                Some(n) if n > 0 => {
                    for c in &cs[i + 1..i + 1 + n] {
                        out.push((*c, base.fg(CODE_FG)));
                    }
                    i += n + 2;
                }
                _ => {
                    out.push((cs[i], base));
                    i += 1;
                }
            }
        } else {
            out.push((cs[i], if bold { base.add_modifier(Modifier::BOLD) } else { base }));
            i += 1;
        }
    }
    decorate(&mut out);
    out
}

/// `path/to/file.rs:123` (optionally `:col`): a source reference worth highlighting.
pub fn is_file_ref(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || parts[0].is_empty() || parts[0].contains("//") {
        return false;
    }
    if !parts[1..].iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    let p = parts[0];
    if !p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-' | '@' | '+' | '~')) {
        return false;
    }
    let ext_ok = p.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && ext.len() <= 8 && !ext.contains('/'));
    ext_ok || p.contains('/')
}

pub fn is_url(tok: &str) -> bool {
    (tok.starts_with("http://") || tok.starts_with("https://")) && tok.len() > 8
}

/// Colour source references (cyan) and underline URLs inside a styled character run.
fn decorate(run: &mut [(char, Style)]) {
    let mut i = 0;
    while i < run.len() {
        if run[i].0.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < run.len() && !run[i].0.is_whitespace() {
            i += 1;
        }
        let (mut a, mut b) = (start, i);
        while a < b && matches!(run[a].0, '(' | '[' | '<' | '"' | '\'' | '`') {
            a += 1;
        }
        while b > a && matches!(run[b - 1].0, ')' | ']' | '>' | ',' | '.' | ';' | ':' | '"' | '\'' | '`' | '!' | '?') {
            b -= 1;
        }
        if a >= b {
            continue;
        }
        let tok: String = run[a..b].iter().map(|(c, _)| *c).collect();
        if is_url(&tok) {
            for (_, st) in &mut run[a..b] {
                *st = st.fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED);
            }
        } else if is_file_ref(&tok) {
            for (_, st) in &mut run[a..b] {
                *st = st.fg(Color::Cyan);
            }
        }
    }
}

fn plain(text: &str) -> String {
    inline(text, Style::new()).into_iter().map(|(c, _)| c).collect()
}

fn spans_of(run: &[(char, Style)]) -> Vec<Span<'static>> {
    let mut v: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut st: Option<Style> = None;
    for (c, s) in run {
        if st != Some(*s) {
            if let Some(prev) = st {
                v.push(Span::styled(std::mem::take(&mut cur), prev));
            }
            st = Some(*s);
        }
        cur.push(*c);
    }
    if let Some(prev) = st {
        v.push(Span::styled(cur, prev));
    }
    v
}

fn width_of(run: &[(char, Style)]) -> usize {
    run.iter().map(|(c, _)| Span::raw(c.to_string()).width()).sum()
}

/// Greedy word wrap of a styled character stream. `first` prefixes the first line, `hang` columns
/// indent the rest. Words wider than the line (URLs, paths) are kept intact on their own line.
fn wrap(stream: &[(char, Style)], width: usize, first: Span<'static>, hang: usize) -> Vec<Line<'static>> {
    let mut words: Vec<Vec<(char, Style)>> = Vec::new();
    let mut cur: Vec<(char, Style)> = Vec::new();
    for &(c, s) in stream {
        if c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push((c, s));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let first_w = first.width();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![first];
    let mut used = first_w;
    let mut has = false;
    for w in words {
        let ww = width_of(&w);
        if has && used + 1 + ww > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            spans = vec![Span::raw(" ".repeat(hang))];
            used = hang;
            has = false;
        }
        if has {
            spans.push(Span::raw(" "));
            used += 1;
        }
        spans.extend(spans_of(&w));
        used += ww;
        has = true;
    }
    lines.push(Line::from(spans));
    lines
}

// ---------- block markdown ----------

fn bullet(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some((indent, rest));
        }
    }
    None
}

fn numbered(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    let d: usize = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if d == 0 || d > 3 {
        return None;
    }
    let rest = &t[d..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let rest = rest.strip_prefix(' ')?;
    Some((indent, t[..d].to_string(), rest))
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let n = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) && t[n..].starts_with(' ') {
        Some((n, t[n..].trim().trim_end_matches('#').trim()))
    } else {
        None
    }
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.matches('|').count() >= 2
}

fn cells(line: &str) -> Vec<String> {
    line.trim().trim_matches('|').split('|').map(|c| plain(c.trim())).collect()
}

fn is_separator(cs: &[String]) -> bool {
    !cs.is_empty() && cs.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

fn pad(s: &str, w: usize) -> String {
    let n = s.chars().count();
    format!("{s}{}", " ".repeat(w.saturating_sub(n)))
}

fn render_table(rows: &[String], width: usize) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut parsed: Vec<Vec<String>> = rows.iter().map(|r| cells(r)).collect();
    let had_sep = parsed.len() > 1 && is_separator(&parsed[1]);
    parsed.retain(|r| !is_separator(r));
    let ncols = parsed.iter().map(Vec::len).max().unwrap_or(0);
    if ncols == 0 {
        return Vec::new();
    }
    let mut widths = vec![1usize; ncols];
    for r in &parsed {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let gap = 3;
    let avail = width.saturating_sub(gap * (ncols - 1)).max(ncols * 4);
    while widths.iter().sum::<usize>() > avail {
        let (i, _) = widths.iter().enumerate().max_by_key(|(_, w)| **w).unwrap();
        if widths[i] <= 4 {
            break;
        }
        widths[i] -= 1;
    }
    let mut out = Vec::new();
    for (ri, r) in parsed.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", dim));
            }
            let cell = r.get(i).map(String::as_str).unwrap_or("");
            let st = if ri == 0 && had_sep { Style::new().bold().fg(Color::LightCyan) } else { Style::new() };
            spans.push(Span::styled(pad(&trunc(cell, *w), *w), st));
        }
        out.push(Line::from(spans));
        if ri == 0 && had_sep {
            let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            out.push(Line::from(Span::styled(rule.join("─┼─"), dim)));
        }
    }
    out
}

/// Render light Markdown to lines no wider than `width` columns.
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(12);
    let dim = Style::new().fg(Color::DarkGray);
    let code_style = Style::new().fg(Color::Gray).bg(Color::Rgb(0x1f, 0x23, 0x2e));
    let src: Vec<&str> = text.lines().collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let blank = |out: &mut Vec<Line<'static>>| {
        if !out.is_empty() && out.last().is_some_and(|l| l.width() > 0) {
            out.push(Line::raw(""));
        }
    };
    let mut i = 0;
    while i < src.len() {
        let line = src[i].trim_end();
        let t = line.trim_start();
        if t.starts_with("```") {
            i += 1;
            while i < src.len() && !src[i].trim_start().starts_with("```") {
                let body = src[i].replace('\t', "    ");
                let shown = trunc(&body, width.saturating_sub(2));
                out.push(Line::from(Span::styled(format!(" {} ", pad(&shown, width.saturating_sub(2))), code_style)));
                i += 1;
            }
            i += 1;
            continue;
        }
        if is_table_row(line) {
            let mut rows = Vec::new();
            while i < src.len() && is_table_row(src[i]) {
                rows.push(src[i].to_string());
                i += 1;
            }
            out.extend(render_table(&rows, width));
            continue;
        }
        i += 1;
        if t.is_empty() {
            blank(&mut out);
        } else if let Some((n, h)) = heading(line) {
            blank(&mut out);
            let color = match n {
                1 => Color::LightCyan,
                2 => Color::Yellow,
                _ => Color::LightBlue,
            };
            let st = Style::new().bold().fg(color);
            out.extend(wrap(&inline(h, st), width, Span::raw(""), 0));
        } else if t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | '_' | '*' | ' ')) && t.chars().filter(|c| *c != ' ').count() >= 3 {
            out.push(Line::from(Span::styled("─".repeat(width.min(40)), dim)));
        } else if let Some((ind, rest)) = bullet(line) {
            let ind = (ind / 2 * 2).min(8);
            let first = Span::styled(format!("{}• ", " ".repeat(ind)), Style::new().fg(Color::Yellow));
            out.extend(wrap(&inline(rest, Style::new()), width, first, ind + 2));
        } else if let Some((ind, num, rest)) = numbered(line) {
            let ind = (ind / 2 * 2).min(8);
            let label = format!("{}{}. ", " ".repeat(ind), num);
            let hang = label.chars().count();
            out.extend(wrap(&inline(rest, Style::new()), width, Span::styled(label, Style::new().fg(Color::Yellow)), hang));
        } else if let Some(q) = t.strip_prefix("> ") {
            out.extend(wrap(&inline(q, dim.add_modifier(Modifier::ITALIC)), width, Span::styled("│ ", dim), 2));
        } else {
            out.extend(wrap(&inline(t, Style::new()), width, Span::raw(""), 0));
        }
    }
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

/// The "Summary" of some content: its first paragraph, or its first three bullets.
pub fn summary_of(content: &str) -> String {
    let lines: Vec<&str> = content.lines().map(str::trim_end).collect();
    let mut i = 0;
    // Skip leading blanks, headings, rules and fences' openers.
    while i < lines.len() && (lines[i].trim().is_empty() || heading(lines[i]).is_some() || lines[i].trim_start().starts_with("---")) {
        i += 1;
    }
    let mut out: Vec<&str> = Vec::new();
    if i < lines.len() && (bullet(lines[i]).is_some() || numbered(lines[i]).is_some()) {
        while i < lines.len() && out.len() < 3 {
            if bullet(lines[i]).is_some() || numbered(lines[i]).is_some() {
                out.push(lines[i]);
            } else if !lines[i].trim().is_empty() && !lines[i].starts_with(' ') {
                break;
            }
            i += 1;
        }
    } else {
        while i < lines.len() && !lines[i].trim().is_empty() && out.len() < 4 && heading(lines[i]).is_none() && !is_table_row(lines[i]) {
            out.push(lines[i]);
            i += 1;
        }
    }
    out.join("\n")
}

/// First readable line of some content (for the inbox card), markers stripped.
pub fn first_line(content: &str) -> String {
    for l in summary_of(content).lines() {
        let t = l.trim();
        let t = bullet(t).map(|(_, r)| r).or_else(|| numbered(t).map(|(_, _, r)| r)).unwrap_or(t);
        if !t.is_empty() {
            return plain(t);
        }
    }
    String::new()
}

// ---------- verdicts ----------

/// The outcome word a report carries (a verdict, a council stance, or a router status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Insufficient,
    Approve,
    ChangesRequested,
    Support,
    Revise,
    Object,
    Ok,
    Error,
    Timeout,
    Blocked,
    NeedsUserInput,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Insufficient => "INSUFFICIENT",
            Verdict::Approve => "APPROVE",
            Verdict::ChangesRequested => "CHANGES_REQUESTED",
            Verdict::Support => "SUPPORT",
            Verdict::Revise => "REVISE",
            Verdict::Object => "OBJECT",
            Verdict::Ok => "OK",
            Verdict::Error => "ERROR",
            Verdict::Timeout => "TIMEOUT",
            Verdict::Blocked => "BLOCKED",
            Verdict::NeedsUserInput => "NEEDS_USER_INPUT",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Verdict::Pass | Verdict::Approve | Verdict::Support | Verdict::Ok => Color::Green,
            Verdict::Fail | Verdict::Object | Verdict::Error | Verdict::Timeout | Verdict::Blocked => Color::LightRed,
            Verdict::Insufficient | Verdict::Revise => Color::Yellow,
            Verdict::ChangesRequested => Color::Rgb(0xFF, 0x8C, 0x1A),
            Verdict::NeedsUserInput => Color::Magenta,
        }
    }

    /// A good outcome (green).
    pub fn good(self) -> bool {
        matches!(self, Verdict::Pass | Verdict::Approve | Verdict::Support | Verdict::Ok)
    }

    /// A bad outcome (the work failed, was refused or never finished).
    pub fn bad(self) -> bool {
        matches!(
            self,
            Verdict::Fail | Verdict::ChangesRequested | Verdict::Object | Verdict::Error | Verdict::Timeout | Verdict::Blocked
        )
    }

    /// Match an upper-case word. `OK`, `ERROR`... only count when `strict` is false (titles and router fields).
    pub fn from_word(w: &str, strict: bool) -> Option<Verdict> {
        let v = match w {
            "PASS" => Verdict::Pass,
            "FAIL" => Verdict::Fail,
            "INSUFFICIENT" => Verdict::Insufficient,
            "APPROVE" => Verdict::Approve,
            "CHANGES_REQUESTED" => Verdict::ChangesRequested,
            "SUPPORT" => Verdict::Support,
            "REVISE" => Verdict::Revise,
            "OBJECT" => Verdict::Object,
            _ if strict => return None,
            "OK" => Verdict::Ok,
            "ERROR" => Verdict::Error,
            "TIMEOUT" => Verdict::Timeout,
            "BLOCKED" => Verdict::Blocked,
            "NEEDS_USER_INPUT" => Verdict::NeedsUserInput,
            _ => return None,
        };
        Some(v)
    }

    /// A coloured "chip": the word on a solid background.
    pub fn chip(self) -> Span<'static> {
        chip(self.label(), self.color())
    }
}

/// A short label on a solid background.
pub fn chip(text: &str, bg: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(Color::Black).bg(bg).bold())
}

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty())
}

/// Find the verdict a report carries: router errors first, then an explicit `VERDICT:` line, the
/// title (`[PASS]`), the opening lines of the text, and finally a clean router `STATUS: OK`.
pub fn detect_verdict(title: &str, p: &Parsed) -> Option<Verdict> {
    if field(p, "NEEDS_USER_INPUT").is_some() {
        return Some(Verdict::NeedsUserInput);
    }
    let status = field(p, "STATUS").map(str::to_ascii_lowercase);
    match status.as_deref() {
        Some("blocked") => return Some(Verdict::Blocked),
        Some(s) if s != "ok" => {
            let code = field(p, "ERROR_CODE").unwrap_or("").to_ascii_lowercase();
            let err = field(p, "ERROR").unwrap_or("").to_ascii_lowercase();
            let timed_out = code.contains("timeout") || err.contains("timeout") || err.contains("timed out");
            return Some(if timed_out { Verdict::Timeout } else { Verdict::Error });
        }
        _ => {}
    }
    for l in p.content.lines() {
        let t = l.trim().trim_start_matches(['#', '*', '>', '-', ' ']);
        if t.len() >= 8 && t[..7].eq_ignore_ascii_case("verdict") {
            if let Some(v) = words(&t[7..]).find_map(|w| Verdict::from_word(&w.to_ascii_uppercase(), true)) {
                return Some(v);
            }
        }
    }
    if let Some(v) = words(title).find_map(|w| Verdict::from_word(w, false)) {
        return Some(v);
    }
    let head: Vec<&str> = p.content.lines().filter(|l| !l.trim().is_empty()).take(8).collect();
    if let Some(v) = head.iter().flat_map(|l| words(l)).find_map(|w| Verdict::from_word(w, true)) {
        return Some(v);
    }
    (status.as_deref() == Some("ok")).then_some(Verdict::Ok)
}

/// "Memo" from "Memo: Review plan [PASS]" (also with a leading ⚠). None for free-form titles.
pub fn doc_type(title: &str) -> Option<String> {
    let t = title.trim_start_matches('\u{26a0}').trim();
    let (ty, _) = t.split_once(": ")?;
    let ty = ty.trim();
    let ok = !ty.is_empty()
        && ty.chars().count() <= 28
        && ty.split_whitespace().count() <= 3
        && ty.chars().next().is_some_and(|c| c.is_uppercase())
        && ty.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '/' | '_'));
    ok.then(|| ty.to_string())
}

/// The subject part of a ROSSO title: no type, no `[VERDICT]` suffix, no ⚠.
pub fn subject_of(title: &str) -> String {
    let t = title.trim_start_matches('\u{26a0}').trim();
    let rest = if doc_type(t).is_some() { t.split_once(": ").map(|x| x.1).unwrap_or(t) } else { t };
    match rest.rfind(" [") {
        Some(i) if rest.ends_with(']') => rest[..i].trim().to_string(),
        _ => rest.trim().to_string(),
    }
}

// ---------- tabs ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderTab {
    #[default]
    Summary,
    Full,
    Brief,
    Raw,
}

impl ReaderTab {
    pub const ALL: [ReaderTab; 4] = [ReaderTab::Summary, ReaderTab::Full, ReaderTab::Brief, ReaderTab::Raw];

    pub fn label(self) -> &'static str {
        match self {
            ReaderTab::Summary => "Summary",
            ReaderTab::Full => "Full",
            ReaderTab::Brief => "Brief",
            ReaderTab::Raw => "Raw",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next(self) -> ReaderTab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> ReaderTab {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

// ---------- key lists ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Major,
    Medium,
    Minor,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Major => "major",
            Severity::Medium => "medium",
            Severity::Minor => "minor",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Severity::Critical => Color::LightRed,
            Severity::Major => Color::Rgb(0xFF, 0x8C, 0x1A),
            Severity::Medium => Color::Yellow,
            Severity::Minor => Color::LightBlue,
        }
    }
}

/// Severity hinted by words in an item ("critical", "major", "high", "medium", "minor", "low", "nit").
pub fn severity_of(text: &str) -> Option<Severity> {
    let lower = text.to_ascii_lowercase();
    let ws: Vec<&str> = words(&lower).collect();
    let has = |names: &[&str]| ws.iter().any(|w| names.contains(w));
    if has(&["critical", "blocker", "severe", "p0"]) {
        Some(Severity::Critical)
    } else if has(&["major", "high", "p1"]) {
        Some(Severity::Major)
    } else if has(&["medium", "moderate", "p2"]) {
        Some(Severity::Medium)
    } else if has(&["minor", "low", "nit", "nits", "trivial", "p3"]) {
        Some(Severity::Minor)
    } else {
        None
    }
}

/// A titled list of short items found in a report.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyList {
    pub title: String,
    pub items: Vec<(String, Option<Severity>)>,
}

const SECTION_WORDS: [&str; 14] = [
    "required changes", "changes required", "findings", "risks", "open issues", "open questions", "next steps", "issues",
    "concerns", "recommendations", "action items", "blockers", "objections", "problems",
];
const MAX_LISTS: usize = 3;

fn section_title(line: &str) -> Option<String> {
    let t = line.trim();
    if t.is_empty() || bullet(t).is_some() || numbered(t).is_some() || is_table_row(t) {
        return None;
    }
    let is_head = heading(t).is_some();
    let bold_only = t.starts_with("**") && t.trim_end_matches(':').ends_with("**");
    let colon = t.ends_with(':') && t.chars().count() <= 48;
    if !(is_head || bold_only || colon) {
        return None;
    }
    let name = heading(t).map(|(_, h)| h).unwrap_or(t);
    let name = plain(name).trim().trim_end_matches(':').trim().to_string();
    let name = name.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | ' ')).to_string();
    let lower = name.to_ascii_lowercase();
    (lower.chars().count() <= 48 && SECTION_WORDS.iter().any(|w| lower.contains(w))).then_some(name)
}

/// Detect the report's key lists: sections like "Findings" or "Required changes", else a numbered
/// run, else the first table. At most three lists.
pub fn key_lists(content: &str) -> Vec<KeyList> {
    let lines: Vec<&str> = content.lines().collect();
    let mut lists: Vec<KeyList> = Vec::new();
    let mut i = 0;
    while i < lines.len() && lists.len() < MAX_LISTS {
        let Some(title) = section_title(lines[i]) else {
            i += 1;
            continue;
        };
        let mut items: Vec<String> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j];
            if l.trim().is_empty() {
                j += 1;
                continue;
            }
            if heading(l).is_some() || section_title(l).is_some() {
                break;
            }
            if let Some((_, rest)) = bullet(l) {
                items.push(plain(rest.trim()));
            } else if let Some((_, _, rest)) = numbered(l) {
                items.push(plain(rest.trim()));
            } else if l.starts_with(' ') && !items.is_empty() {
                if let Some(last) = items.last_mut() {
                    last.push(' ');
                    last.push_str(&plain(l.trim()));
                }
            } else if !items.is_empty() {
                break;
            }
            j += 1;
        }
        if !items.is_empty() {
            let items = items.into_iter().map(|t| { let s = severity_of(&t); (t, s) }).collect();
            lists.push(KeyList { title, items });
            i = j;
        } else {
            i += 1;
        }
    }
    if lists.is_empty() {
        // A run of numbered items.
        let mut run: Vec<String> = Vec::new();
        for l in &lines {
            if let Some((_, _, rest)) = numbered(l) {
                run.push(plain(rest.trim()));
            } else if !l.trim().is_empty() && !l.starts_with(' ') && run.len() < 2 {
                run.clear();
            } else if !l.trim().is_empty() && !l.starts_with(' ') {
                break;
            }
        }
        if run.len() >= 2 {
            let items = run.into_iter().map(|t| { let s = severity_of(&t); (t, s) }).collect();
            lists.push(KeyList { title: "Numbered items".into(), items });
        }
    }
    if lists.is_empty() {
        let rows: Vec<String> = lines.iter().skip_while(|l| !is_table_row(l)).take_while(|l| is_table_row(l)).map(|l| l.to_string()).collect();
        let mut parsed: Vec<Vec<String>> = rows.iter().map(|r| cells(r)).collect();
        let had_head = parsed.len() > 1 && is_separator(&parsed[1]);
        parsed.retain(|r| !is_separator(r));
        if had_head && !parsed.is_empty() {
            parsed.remove(0);
        }
        let items: Vec<(String, Option<Severity>)> =
            parsed.iter().map(|r| r.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" · ")).filter(|t| !t.is_empty()).map(|t| { let s = severity_of(&t); (t, s) }).collect();
        if !items.is_empty() {
            lists.push(KeyList { title: "Table".into(), items });
        }
    }
    lists
}

/// "Critical: fix the lock" -> "fix the lock" when the severity chip already says it.
fn strip_severity_word(text: &str, sev: Severity) -> &str {
    let l = sev.label();
    if text.len() > l.len() && text.is_char_boundary(l.len()) && text[..l.len()].eq_ignore_ascii_case(l) {
        let rest = text[l.len()..].trim_start_matches([':', '-', ' ']).trim_start();
        if !rest.is_empty() && text[l.len()..].starts_with([':', '-', ' ']) {
            return rest;
        }
    }
    text
}

/// Compact bullet cards for key lists (at most 8 items each), severity-coloured.
pub fn render_key_lists(lists: &[KeyList], width: usize) -> Vec<Line<'static>> {
    const SHOWN: usize = 8;
    let dim = Style::new().fg(Color::DarkGray);
    let mut out: Vec<Line<'static>> = Vec::new();
    for (n, l) in lists.iter().enumerate() {
        if n > 0 {
            out.push(Line::raw(""));
        }
        out.push(Line::from(vec![
            Span::styled("▎", Style::new().fg(Color::Yellow)),
            Span::styled(format!(" {} ", l.title), Style::new().fg(Color::Yellow).bold()),
            Span::styled(format!("({})", l.items.len()), dim),
        ]));
        for (text, sev) in l.items.iter().take(SHOWN) {
            let bar = sev.map(Severity::color).unwrap_or(Color::DarkGray);
            let mut first = vec![Span::styled("  ▌ ", Style::new().fg(bar))];
            if let Some(s) = sev {
                first.push(Span::styled(format!("{} ", s.label()), Style::new().fg(s.color()).bold()));
            }
            let lead: usize = first.iter().map(|s| s.width()).sum();
            let shown = match sev {
                Some(sv) => strip_severity_word(text, *sv),
                None => text.as_str(),
            };
            let mut lines = wrap(&inline(shown, Style::new()), width, Span::raw(""), lead);
            if let Some(f) = lines.first_mut() {
                f.spans.remove(0);
                for (k, sp) in first.into_iter().enumerate() {
                    f.spans.insert(k, sp);
                }
            }
            out.extend(lines);
        }
        if l.items.len() > SHOWN {
            out.push(Line::from(Span::styled(format!("    … {} more (see Full)", l.items.len() - SHOWN), dim)));
        }
    }
    out
}

// ---------- the reader's content ----------

fn dim_line(t: &str) -> Line<'static> {
    Line::from(Span::styled(t.to_string(), Style::new().fg(Color::DarkGray)))
}

fn status_and_alerts(p: &Parsed, w: usize) -> Vec<Line<'static>> {
    let mut v = Vec::new();
    if let Some((st, col)) = status_line(p) {
        v.push(Line::from(Span::styled(st, Style::new().fg(col).bold())));
    }
    for (msg, col) in alert_lines(p) {
        v.extend(render_markdown(&msg, w).into_iter().map(|l| l.patch_style(Style::new().fg(col).bold())));
    }
    v
}

/// What the sender was asked: the brief, or for a brief itself, its text.
fn brief_text(r: &Report) -> String {
    if r.kind == ReportKind::Task && r.brief.trim().is_empty() {
        r.body.trim().to_string()
    } else {
        r.brief.trim().to_string()
    }
}

/// Lines for one reader tab, wrapped to `width` columns.
pub fn tab_lines(r: &Report, tab: ReaderTab, width: usize) -> Vec<Line<'static>> {
    let w = width.clamp(12, MAX_WIDTH);
    let parsed = parse_body(&r.body);
    let content = parsed.content.clone();
    let mut v: Vec<Line<'static>> = Vec::new();
    match tab {
        ReaderTab::Summary => {
            v.extend(status_and_alerts(&parsed, w));
            let summary = summary_of(&content);
            if !summary.trim().is_empty() {
                if !v.is_empty() {
                    v.push(Line::raw(""));
                }
                v.extend(render_markdown(&summary, w));
            }
            let lists = key_lists(&content);
            if !lists.is_empty() {
                v.push(Line::raw(""));
                v.extend(render_key_lists(&lists, w));
            }
            if v.is_empty() {
                v.push(dim_line("(no text captured)"));
            }
        }
        ReaderTab::Full => {
            v.extend(status_and_alerts(&parsed, w));
            if !content.trim().is_empty() {
                if !v.is_empty() {
                    v.push(Line::raw(""));
                }
                v.extend(render_markdown(&content, w));
            }
            if v.is_empty() {
                v.push(dim_line("(no text captured)"));
            }
        }
        ReaderTab::Brief => {
            let b = brief_text(r);
            if b.is_empty() {
                v.push(dim_line("(no brief captured: it is filled in when the report comes back)"));
            } else {
                v.extend(render_markdown(&b, w).into_iter().map(|l| l.patch_style(Style::new().fg(Color::Gray))));
            }
        }
        ReaderTab::Raw => {
            for l in r.body.lines() {
                let l = l.replace('\t', "    ");
                let cs: Vec<char> = l.chars().collect();
                if cs.is_empty() {
                    v.push(Line::raw(""));
                }
                for chunk in cs.chunks(w.max(1)) {
                    v.push(Line::raw(chunk.iter().collect::<String>()));
                }
            }
            if v.is_empty() {
                v.push(dim_line("(empty)"));
            }
        }
    }
    v
}

/// Plain text of a tab, for the clipboard.
pub fn tab_plain(r: &Report, tab: ReaderTab) -> String {
    let parsed = parse_body(&r.body);
    match tab {
        ReaderTab::Summary => {
            let mut parts: Vec<String> = Vec::new();
            if let Some((st, _)) = status_line(&parsed) {
                parts.push(st);
            }
            parts.extend(alert_lines(&parsed).into_iter().map(|(m, _)| m));
            let s = summary_of(&parsed.content);
            if !s.trim().is_empty() {
                parts.push(s);
            }
            for l in key_lists(&parsed.content) {
                let items: Vec<String> = l.items.iter().map(|(t, _)| format!("- {t}")).collect();
                parts.push(format!("{}:\n{}", l.title, items.join("\n")));
            }
            parts.join("\n\n")
        }
        ReaderTab::Full => parsed.content,
        ReaderTab::Brief => brief_text(r),
        ReaderTab::Raw => r.body.clone(),
    }
}

/// The whole report as a Markdown document (what `w` saves).
pub fn report_markdown(r: &Report, now_ms: u64) -> String {
    let parsed = parse_body(&r.body);
    let mut s = format!("# {}\n\n", r.title);
    s.push_str(&format!("- **From:** {} → {}\n", r.from_label, r.to_label));
    s.push_str(&format!("- **Kind:** {}\n", kind_name(r.kind)));
    s.push_str(&format!("- **Age when saved:** {} ago\n", fmt_ago(now_ms, r.at_ms)));
    if let Some(v) = detect_verdict(&r.title, &parsed) {
        s.push_str(&format!("- **Verdict:** {}\n", v.label()));
    }
    if let Some((st, _)) = status_line(&parsed) {
        s.push_str(&format!("- **Status:** {st}\n"));
    }
    let b = brief_text(r);
    if r.kind != ReportKind::Task && !b.is_empty() {
        s.push_str(&format!("\n## Brief\n\n{b}\n"));
    }
    s.push_str(&format!("\n## {}\n\n{}\n", if r.kind == ReportKind::Task { "Brief" } else { "Report" }, parsed.content.trim()));
    s
}

pub fn kind_name(k: ReportKind) -> &'static str {
    match k {
        ReportKind::Task => "brief",
        ReportKind::Report => "report",
        ReportKind::Result => "result",
    }
}

pub fn kind_tag_color(k: ReportKind) -> Color {
    match k {
        ReportKind::Task => Color::Gray,
        ReportKind::Report => Color::Yellow,
        ReportKind::Result => Color::LightBlue,
    }
}

/// Colour of a party in a hand-off: its agent colour, the boss in light yellow.
fn party_color(scene: &Scene, id: Option<&str>) -> Color {
    match id {
        None => Color::LightYellow,
        Some(id) => scene.agents.get(id).map(|a| kind_color(a.kind)).unwrap_or(Color::Gray),
    }
}

/// The reader's header card: verdict / type / kind chips, the title, and who → whom, when, how long.
pub fn header_card(r: &Report, scene: &Scene, now_ms: u64, width: usize) -> Vec<Line<'static>> {
    let parsed = parse_body(&r.body);
    let dim = Style::new().fg(Color::DarkGray);
    let mut chips: Vec<Span<'static>> = Vec::new();
    if let Some(v) = detect_verdict(&r.title, &parsed) {
        chips.push(v.chip());
        chips.push(Span::raw(" "));
    }
    if r.title.starts_with('\u{26a0}') {
        chips.push(chip("ESCALATION", Color::LightRed));
        chips.push(Span::raw(" "));
    }
    if let Some(t) = doc_type(&r.title) {
        chips.push(chip(&t, Color::Rgb(0x8F, 0x7B, 0xD8)));
        chips.push(Span::raw(" "));
    }
    chips.push(chip(kind_name(r.kind), kind_tag_color(r.kind)));
    let mut meta: Vec<Span<'static>> = vec![
        Span::styled(r.from_label.clone(), Style::new().fg(party_color(scene, Some(&r.from))).bold()),
        Span::styled(" → ", dim),
        Span::styled(r.to_label.clone(), Style::new().fg(party_color(scene, r.to.as_deref())).bold()),
        Span::styled(format!("  ·  {} ago", fmt_ago(now_ms, r.at_ms)), dim),
    ];
    if let Some(ms) = field(&parsed, "DURATION_MS").and_then(|v| v.parse::<u64>().ok()) {
        meta.push(Span::styled(format!("  ·  took {}", human_duration(ms)), dim));
    }
    if let Some(m) = field(&parsed, "MODEL") {
        meta.push(Span::styled(format!("  ·  {}", m.strip_prefix("gemini-4-").unwrap_or(m)), dim));
    }
    vec![
        Line::from(chips),
        Line::from(Span::styled(trunc(&r.title, width), Style::new().bold())),
        Line::from(meta),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER: &str = "STATUS: OK\nACCOUNT: main\nMODEL: gemini-4-neon-xhigh\nROLE: council-architect\nQUEUED_MS: 12\nSLOT: 1\nCAPACITY: 3\nFREE_MB: 5000\nPER_PROC_MB: 900\nRESERVED_MB: 0\nPRESSURE: normal\nDURATION_MS: 238000\nRESPONSE:\n# Plan\n\nDo the thing.\n- one\n- two\n";

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn router_block_collapses_to_one_line() {
        let p = parse_body(ROUTER);
        assert_eq!(p.fields.len(), 12);
        assert!(p.content.starts_with("# Plan"));
        let (s, c) = status_line(&p).unwrap();
        assert_eq!(s, "✓ OK · Chief Architect (neon-xhigh) · 3 m 58 s");
        assert_eq!(c, Color::Green);
    }

    #[test]
    fn errors_and_questions_are_prominent() {
        let p = parse_body("STATUS: ERROR\nERROR_CODE: timeout\nERROR: ran out\n of time\nDURATION_MS: 570000\n");
        let (s, c) = status_line(&p).unwrap();
        assert!(s.starts_with("✗ ERROR · timeout"), "{s}");
        assert_eq!(c, Color::LightRed);
        assert_eq!(alert_lines(&p)[0].0, "Error: ran out of time");
        let q = parse_body("STATUS: BLOCKED\nNEEDS_USER_INPUT: which repo?\n");
        assert!(status_line(&q).unwrap().0.starts_with("? Needs your answer"));
        assert_eq!(alert_lines(&q)[0].0, "Needs your input: which repo?");
    }

    #[test]
    fn plain_text_is_untouched() {
        let p = parse_body("Hello: world\nsecond");
        assert!(p.fields.is_empty());
        assert_eq!(p.content, "Hello: world\nsecond");
        assert!(status_line(&p).is_none());
    }

    #[test]
    fn markdown_blocks_render() {
        let md = "# Title\nsome **bold** and `code` here\n- item one that is long enough to wrap around the narrow width\n1. first\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nlet x = 1;\n```";
        let t = text(&render_markdown(md, 30));
        assert_eq!(t[0], "Title");
        assert_eq!(t[1], "some bold and code here");
        assert!(t[2].starts_with("• item one"));
        assert!(t[3].starts_with("  ") && !t[3].starts_with("• "), "hanging indent: {:?}", t[3]);
        assert!(t.iter().any(|l| l.starts_with("1. first")));
        assert!(t.iter().any(|l| l.starts_with("a │ b") || l.starts_with("a  │ b")), "{t:?}");
        assert!(t.iter().all(|l| !l.contains('|') && !l.contains("```") && !l.contains('`') && !l.contains("**")));
        assert!(t.iter().any(|l| l.contains("let x = 1;")));
        assert!(t.iter().all(|l| l.chars().count() <= 30));
    }

    #[test]
    fn long_urls_stay_intact() {
        let url = "https://example.com/a/very/long/path/that/does/not/fit/in/the/line";
        let t = text(&render_markdown(&format!("see {url} now"), 20));
        assert!(t.iter().any(|l| l.trim() == url), "{t:?}");
    }

    #[test]
    fn summary_takes_first_paragraph_or_three_bullets() {
        assert_eq!(summary_of("# H\n\nfirst para\nstill\n\nsecond"), "first para\nstill");
        assert_eq!(summary_of("- a\n- b\n- c\n- d"), "- a\n- b\n- c");
        assert_eq!(first_line("- **a** thing\n- b"), "a thing");
    }

    fn v(title: &str, body: &str) -> Option<Verdict> {
        detect_verdict(title, &parse_body(body))
    }

    #[test]
    fn verdict_chips_are_detected_from_text() {
        assert_eq!(v("Inspection: x [PASS]", "all fine"), Some(Verdict::Pass));
        assert_eq!(v("t", "VERDICT: FAIL\n3 tests broke"), Some(Verdict::Fail));
        assert_eq!(v("t", "**Verdict:** INSUFFICIENT"), Some(Verdict::Insufficient));
        assert_eq!(v("t", "# Review\nAPPROVE\nlooks good"), Some(Verdict::Approve));
        assert_eq!(v("Review [CHANGES_REQUESTED]", ""), Some(Verdict::ChangesRequested));
        assert_eq!(v("Critique [SUPPORT]", ""), Some(Verdict::Support));
        assert_eq!(v("Critique [REVISE]", ""), Some(Verdict::Revise));
        assert_eq!(v("Critique [OBJECT]", ""), Some(Verdict::Object));
        assert_eq!(v("t", "STATUS: OK\nRESPONSE:\nplain text"), Some(Verdict::Ok));
        assert_eq!(v("t", "STATUS: ERROR\nERROR_CODE: timeout\nERROR: ran out\n"), Some(Verdict::Timeout));
        assert_eq!(v("t", "STATUS: ERROR\nERROR: boom\n"), Some(Verdict::Error));
        assert_eq!(v("t", "STATUS: BLOCKED\n"), Some(Verdict::Blocked));
        assert_eq!(v("t", "STATUS: BLOCKED\nNEEDS_USER_INPUT: which repo?\n"), Some(Verdict::NeedsUserInput));
        // An OK status with a real verdict inside: the verdict wins.
        assert_eq!(v("t", "STATUS: OK\nRESPONSE:\nVERDICT: PASS"), Some(Verdict::Pass));
        assert_eq!(v("Turn done", "nothing special here, ok?"), None);
        assert_eq!(Verdict::Pass.color(), Color::Green);
        assert_eq!(Verdict::Fail.color(), Color::LightRed);
        assert_eq!(Verdict::Insufficient.color(), Color::Yellow);
        assert_eq!(Verdict::NeedsUserInput.color(), Color::Magenta);
        assert_ne!(Verdict::ChangesRequested.color(), Color::LightRed);
        assert!(Verdict::Pass.good() && Verdict::Timeout.bad() && !Verdict::Revise.bad());
    }

    #[test]
    fn title_parts() {
        assert_eq!(doc_type("Memo: Review plan [PASS]").as_deref(), Some("Memo"));
        assert_eq!(doc_type("\u{26a0} Escalation: R-1").as_deref(), Some("Escalation"));
        assert_eq!(doc_type("Turn done"), None);
        assert_eq!(doc_type("cc·DICE: hi"), None);
        assert_eq!(subject_of("Memo: Review plan [PASS]"), "Review plan");
        assert_eq!(subject_of("Plain title"), "Plain title");
    }

    fn span_style(lines: &[Line<'static>], needle: &str) -> Option<Style> {
        lines.iter().flat_map(|l| l.spans.iter()).find(|s| s.content.contains(needle)).map(|s| s.style)
    }

    #[test]
    fn file_line_refs_are_cyan_and_urls_underlined() {
        assert!(is_file_ref("crates/x/y.rs:123"));
        assert!(is_file_ref("src/main.rs:10:5"));
        assert!(is_file_ref("lib.rs:7"));
        assert!(!is_file_ref("12:30"));
        assert!(!is_file_ref("https://example.com:8080"));
        assert!(!is_file_ref("note:text"));
        assert!(!is_file_ref("main.rs"));
        let lines = render_markdown("see crates/x/y.rs:123, and (src/a.rs:9) or https://example.com/a?b=1. done", 100);
        let st = span_style(&lines, "crates/x/y.rs:123").expect("ref span");
        assert_eq!(st.fg, Some(Color::Cyan));
        assert_eq!(span_style(&lines, "src/a.rs:9").unwrap().fg, Some(Color::Cyan));
        let url = span_style(&lines, "https://example.com/a?b=1").expect("url span");
        assert!(url.add_modifier.contains(Modifier::UNDERLINED));
        assert!(!span_style(&lines, "done").unwrap().add_modifier.contains(Modifier::UNDERLINED));
        // The trailing comma is not part of the highlighted reference.
        assert!(lines.iter().flat_map(|l| l.spans.iter()).any(|s| s.content == "crates/x/y.rs:123"));
        // Inside inline code too.
        let lines = render_markdown("at `a/b.rs:5` now", 100);
        assert_eq!(span_style(&lines, "a/b.rs:5").unwrap().fg, Some(Color::Cyan));
    }

    #[test]
    fn key_lists_are_found_with_severity() {
        let md = "# Review\nIntro text.\n\n## Required changes\n1. Critical: fix the lock\n2. Minor: rename `x`\n   (wrapped line)\n\n## Notes\nplain\n\n**Risks**\n- high load on cache\n";
        let l = key_lists(md);
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!(l[0].title, "Required changes");
        assert_eq!(l[0].items[0], ("Critical: fix the lock".to_string(), Some(Severity::Critical)));
        assert_eq!(l[0].items[1].1, Some(Severity::Minor));
        assert!(l[0].items[1].0.ends_with("(wrapped line)"));
        assert_eq!(l[1].title, "Risks");
        assert_eq!(l[1].items[0].1, Some(Severity::Major));
        // Fallbacks: a numbered run, then a table.
        let l = key_lists("steps:\n1. a\n2. b\n");
        assert_eq!(l[0].title, "Numbered items");
        let l = key_lists("| a | b |\n|---|---|\n| x | medium thing |\n");
        assert_eq!(l[0].title, "Table");
        assert_eq!(l[0].items, vec![("x · medium thing".to_string(), Some(Severity::Medium))]);
        assert!(key_lists("just prose\nmore prose").is_empty());
        let cards = render_key_lists(&key_lists(md), 40);
        let t = text(&cards);
        assert!(t[0].contains("Required changes") && t[0].contains("(2)"), "{t:?}");
        assert!(t[1].contains("critical") && t[1].contains("fix the lock") && !t[1].contains("critical Critical"), "{t:?}");
        assert!(t.iter().all(|l| l.chars().count() <= 40), "{t:?}");
    }

    fn report(kind: ReportKind, title: &str, body: &str, brief: &str) -> Report {
        Report {
            id: 4, at_ms: 1_000, kind, from: "cc:a".into(), from_label: "A".into(), to: None, to_label: "you".into(),
            title: title.into(), body: body.into(), brief: brief.into(),
        }
    }

    #[test]
    fn tabs_cycle_and_hold_the_right_text() {
        assert_eq!(ReaderTab::Summary.next(), ReaderTab::Full);
        assert_eq!(ReaderTab::Raw.next(), ReaderTab::Summary);
        assert_eq!(ReaderTab::Summary.prev(), ReaderTab::Raw);
        let body = "STATUS: OK\nMODEL: gemini-4-neon-high\nRESPONSE:\nFirst para.\n\n## Findings\n- one\n- two\n";
        let r = report(ReportKind::Report, "Review: x [PASS]", body, "what was asked");
        let txt = |tab| text(&tab_lines(&r, tab, 60)).join("\n");
        assert!(txt(ReaderTab::Summary).contains("✓ OK") && txt(ReaderTab::Summary).contains("First para.") && txt(ReaderTab::Summary).contains("Findings"));
        assert!(txt(ReaderTab::Full).contains("• one") && !txt(ReaderTab::Full).contains("what was asked"));
        assert_eq!(txt(ReaderTab::Brief), "what was asked");
        assert!(txt(ReaderTab::Raw).contains("STATUS: OK") && txt(ReaderTab::Raw).contains("## Findings"));
        assert_eq!(tab_plain(&r, ReaderTab::Raw), body);
        assert_eq!(tab_plain(&r, ReaderTab::Brief), "what was asked");
        assert!(tab_plain(&r, ReaderTab::Full).starts_with("First para."));
        assert!(tab_plain(&r, ReaderTab::Summary).contains("- one"));
        // A brief (Task) is its own text.
        let t = report(ReportKind::Task, "Memo: y", "do the thing", "");
        assert_eq!(tab_plain(&t, ReaderTab::Brief), "do the thing");
        // Empty bodies degrade gracefully.
        let e = report(ReportKind::Report, "Empty", "", "");
        assert!(text(&tab_lines(&e, ReaderTab::Summary, 60))[0].contains("no text captured"));
        assert!(text(&tab_lines(&e, ReaderTab::Brief, 60))[0].contains("no brief captured"));
    }

    #[test]
    fn saved_markdown_has_title_verdict_and_body() {
        let r = report(ReportKind::Report, "Inspection: x [PASS]", "STATUS: OK\nRESPONSE:\nAll green.", "run tests");
        let md = report_markdown(&r, 61_000);
        assert!(md.starts_with("# Inspection: x [PASS]\n"), "{md}");
        assert!(md.contains("**Verdict:** PASS") && md.contains("**From:** A → you") && md.contains("**Kind:** report"), "{md}");
        assert!(md.contains("## Brief\n\nrun tests") && md.contains("## Report\n\nAll green."), "{md}");
    }

    #[test]
    fn header_card_has_chips_and_meta() {
        let scene = Scene::default();
        let body = "STATUS: OK\nMODEL: gemini-4-neon-high\nDURATION_MS: 65000\nRESPONSE:\nVERDICT: PASS";
        let r = report(ReportKind::Result, "Inspection: x", body, "");
        let card = text(&header_card(&r, &scene, 11_000, 80));
        assert_eq!(card.len(), 3);
        assert!(card[0].contains(" PASS ") && card[0].contains(" Inspection ") && card[0].contains(" result "), "{card:?}");
        assert!(card[2].contains("A → you") && card[2].contains("10s ago") && card[2].contains("took 1 m 05 s") && card[2].contains("neon-high"), "{card:?}");
    }
}
