//! The report reader: turns an agent's raw hand-off text into something friendly to read.
//! - collapses the agy router header (STATUS / MODEL / ROLE / ... / RESPONSE:) into one status line,
//! - renders light Markdown (headings, bullets, numbered lists, **bold**, `code`, fences, tables),
//! - wraps to a fixed column width itself, so scrolling can count the rendered lines exactly.
use super::fmt::trunc;
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
    out
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
}
