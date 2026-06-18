//! A small, tolerant Markdown renderer that turns assistant replies into
//! width-wrapped, styled ratatui [`Line`]s.
//!
//! It deliberately covers only what chat models routinely emit — headings,
//! bold/italic/inline-code, fenced code blocks, bullet/numbered lists,
//! blockquotes, and horizontal rules — and degrades gracefully on anything
//! else (including the half-finished markup seen mid-stream).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const CODE_FG: Color = Color::Rgb(171, 178, 191);
const CODE_BG: Color = Color::Rgb(40, 44, 52);
const INLINE_CODE_FG: Color = Color::Rgb(229, 192, 123);

/// Render Markdown `content` into styled lines already wrapped to `width`.
pub fn render(content: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;

    for raw in content.split('\n') {
        let trimmed = raw.trim_start();

        if is_fence(trimmed) {
            in_code = !in_code;
            continue;
        }
        if in_code {
            push_code_line(&mut out, raw, width);
            continue;
        }
        if trimmed.is_empty() {
            out.push(Line::from(String::new()));
            continue;
        }
        if is_hr(trimmed) {
            out.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        if let Some(text) = heading(trimmed) {
            let style = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            flow(&mut out, parse_inline(text, style), width, "");
            continue;
        }
        if let Some(text) = blockquote(trimmed) {
            let base = Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::ITALIC);
            let mut tokens = vec![("│ ".to_string(), Style::default().fg(Color::DarkGray))];
            tokens.extend(parse_inline(text, base));
            flow(&mut out, tokens, width, "│ ");
            continue;
        }
        if let Some((marker, text)) = list_item(trimmed) {
            let indent = " ".repeat(marker.chars().count());
            let mut tokens = vec![(marker, Style::default().fg(Color::Yellow))];
            tokens.extend(parse_inline(text, Style::default()));
            flow(&mut out, tokens, width, &indent);
            continue;
        }

        flow(&mut out, parse_inline(trimmed, Style::default()), width, "");
    }

    out
}

// --- block helpers ---------------------------------------------------------

fn is_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

fn is_hr(line: &str) -> bool {
    if line.contains('|') {
        return false; // avoid mistaking a table separator row for a rule
    }
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.chars().count() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

/// Strip the leading `#`s of an ATX heading, returning the heading text.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.is_empty() {
            return Some("");
        }
        if let Some(stripped) = rest.strip_prefix(' ') {
            return Some(stripped.trim_start());
        }
    }
    None
}

fn blockquote(line: &str) -> Option<&str> {
    line.strip_prefix("> ").or_else(|| line.strip_prefix('>'))
}

/// Detect a list item, returning the display marker and the remaining text.
fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ['-', '*', '+'] {
        if let Some(rest) = line.strip_prefix(bullet)
            && let Some(rest) = rest.strip_prefix(' ')
        {
            return Some(("• ".to_string(), rest));
        }
    }
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty()
        && let Some(rest) = line[digits.len()..].strip_prefix(". ")
    {
        return Some((format!("{digits}. "), rest));
    }
    None
}

// --- inline parsing --------------------------------------------------------

/// Split a line of text into styled runs, resolving `code`, **bold**, and
/// *italic* spans. Unterminated markers are left as literal text.
fn parse_inline(text: &str, base: Style) -> Vec<(String, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens: Vec<(String, Style)> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // `inline code`
        if c == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            flush(&mut tokens, &mut buf, base);
            let code: String = chars[i + 1..end].iter().collect();
            tokens.push((code, base.fg(INLINE_CODE_FG).bg(CODE_BG)));
            i = end + 1;
            continue;
        }

        // **bold** / __bold__
        if let Some((end, fence_len)) = emphasis_span(&chars, i, 2) {
            flush(&mut tokens, &mut buf, base);
            let inner: String = chars[i + fence_len..end].iter().collect();
            tokens.push((inner, base.add_modifier(Modifier::BOLD)));
            i = end + fence_len;
            continue;
        }

        // *italic* / _italic_
        if let Some((end, fence_len)) = emphasis_span(&chars, i, 1) {
            flush(&mut tokens, &mut buf, base);
            let inner: String = chars[i + fence_len..end].iter().collect();
            tokens.push((inner, base.add_modifier(Modifier::ITALIC)));
            i = end + fence_len;
            continue;
        }

        buf.push(c);
        i += 1;
    }

    flush(&mut tokens, &mut buf, base);
    tokens
}

/// If an emphasis run of `len` delimiters (`*`/`_`) opens at `start`, return the
/// closing delimiter index and the delimiter length. Underscore runs require
/// non-alphanumeric boundaries so `snake_case` words are left untouched.
fn emphasis_span(chars: &[char], start: usize, len: usize) -> Option<(usize, usize)> {
    let marker = *chars.get(start)?;
    if marker != '*' && marker != '_' {
        return None;
    }
    // All `len` opening chars must match the marker.
    for offset in 0..len {
        if chars.get(start + offset) != Some(&marker) {
            return None;
        }
    }
    // A longer run exists (e.g. `**` when len==1) — let the wider match win.
    if chars.get(start + len) == Some(&marker) {
        return None;
    }
    if marker == '_' && !left_boundary(chars, start) {
        return None;
    }

    let end = find_run(chars, start + len, marker, len)?;
    if end <= start + len {
        return None; // empty content
    }
    if marker == '_' && !right_boundary(chars, end + len) {
        return None;
    }
    Some((end, len))
}

fn find_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == target)
}

/// Find the start index of a run of exactly `len` `target` chars at/after `start`.
fn find_run(chars: &[char], start: usize, target: char, len: usize) -> Option<usize> {
    let mut i = start;
    while i + len <= chars.len() {
        if (0..len).all(|o| chars[i + o] == target) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn left_boundary(chars: &[char], i: usize) -> bool {
    i == 0 || !chars[i - 1].is_alphanumeric()
}

fn right_boundary(chars: &[char], i: usize) -> bool {
    chars.get(i).is_none_or(|c| !c.is_alphanumeric())
}

fn flush(tokens: &mut Vec<(String, Style)>, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        tokens.push((std::mem::take(buf), style));
    }
}

// --- layout ----------------------------------------------------------------

/// A fenced-code line: rendered verbatim on a solid background, hard-wrapped.
fn push_code_line(out: &mut Vec<Line<'static>>, raw: &str, width: usize) {
    let style = Style::default().fg(CODE_FG).bg(CODE_BG);
    let chars: Vec<char> = raw.chars().collect();
    if chars.is_empty() {
        out.push(Line::from(Span::styled(" ".repeat(width), style)));
        return;
    }
    for chunk in chars.chunks(width) {
        let mut text: String = chunk.iter().collect();
        let pad = width.saturating_sub(chunk.len());
        text.push_str(&" ".repeat(pad));
        out.push(Line::from(Span::styled(text, style)));
    }
}

/// Word-wrap styled `tokens` to `width`, prefixing continuation lines with
/// `cont_prefix` (used to indent wrapped list items and blockquotes). Spaces
/// keep their original style so a multi-word styled run (e.g. inline code)
/// stays visually continuous.
fn flow(out: &mut Vec<Line<'static>>, tokens: Vec<(String, Style)>, width: usize, cont_prefix: &str) {
    let prefix: Vec<(char, Style)> = cont_prefix
        .chars()
        .map(|c| (c, Style::default().fg(Color::DarkGray)))
        .collect();
    let prefix_w = cont_prefix.chars().count();
    let chunk_w = width.saturating_sub(prefix_w).max(1);

    // Split the styled stream into alternating word / whitespace segments,
    // hard-splitting any word longer than the usable width.
    let mut segments: Vec<(bool, Vec<(char, Style)>)> = Vec::new();
    let mut seg: Vec<(char, Style)> = Vec::new();
    let mut seg_space = false;
    for (text, style) in tokens {
        for c in text.chars() {
            let is_space = c == ' ';
            if seg.is_empty() {
                seg_space = is_space;
            } else if is_space != seg_space {
                push_segment(&mut segments, seg_space, std::mem::take(&mut seg), chunk_w);
                seg_space = is_space;
            }
            seg.push((c, style));
        }
    }
    if !seg.is_empty() {
        push_segment(&mut segments, seg_space, seg, chunk_w);
    }

    let mut line: Vec<(char, Style)> = Vec::new();
    let mut line_w = 0usize;
    for (is_space, seg) in segments {
        let len = seg.len();
        if is_space {
            if line_w == 0 {
                continue; // no leading whitespace on a line
            }
            if line_w + len <= width {
                line.extend(seg);
                line_w += len;
            } else {
                out.push(to_line(std::mem::take(&mut line)));
                line.extend(prefix.iter().copied());
                line_w = prefix_w;
            }
        } else {
            if line_w != 0 && line_w + len > width {
                out.push(to_line(std::mem::take(&mut line)));
                line.extend(prefix.iter().copied());
                line_w = prefix_w;
            }
            line.extend(seg);
            line_w += len;
        }
    }

    // Push the trailing line unless it is only a leftover continuation prefix.
    if line.len() > prefix.len() {
        out.push(to_line(line));
    }
}

/// Append a segment, breaking over-long words into width-sized pieces.
fn push_segment(
    segments: &mut Vec<(bool, Vec<(char, Style)>)>,
    is_space: bool,
    seg: Vec<(char, Style)>,
    chunk_w: usize,
) {
    if is_space || seg.len() <= chunk_w {
        segments.push((is_space, seg));
        return;
    }
    for chunk in seg.chunks(chunk_w) {
        segments.push((false, chunk.to_vec()));
    }
}

/// Merge a run of styled chars into spans of contiguous equal style.
fn to_line(chars: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut current: Option<Style> = None;
    for (c, style) in chars {
        match current {
            Some(s) if s == style => buf.push(c),
            _ => {
                if let Some(s) = current {
                    spans.push(Span::styled(std::mem::take(&mut buf), s));
                }
                buf.push(c);
                current = Some(style);
            }
        }
    }
    if let Some(s) = current {
        spans.push(Span::styled(buf, s));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line's spans back into plain text.
    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Find the span containing `needle` and return its style.
    fn style_of(line: &Line, needle: &str) -> Style {
        line.spans
            .iter()
            .find(|s| s.content.contains(needle))
            .map(|s| s.style)
            .unwrap()
    }

    #[test]
    fn bold_and_italic_become_modifiers() {
        let lines = render("**bold** and *soft*", 80);
        assert_eq!(text(&lines[0]), "bold and soft");
        assert!(style_of(&lines[0], "bold").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&lines[0], "soft").add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn inline_code_is_recolored() {
        let lines = render("run `cargo build` now", 80);
        assert_eq!(text(&lines[0]), "run cargo build now");
        assert_eq!(style_of(&lines[0], "cargo build").fg, Some(INLINE_CODE_FG));
    }

    #[test]
    fn heading_strips_hashes_and_emphasizes() {
        let lines = render("## Section title", 80);
        assert_eq!(text(&lines[0]), "Section title");
        assert!(style_of(&lines[0], "Section").add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bullets_and_numbers_get_markers() {
        let bullet = render("- first", 80);
        assert_eq!(text(&bullet[0]), "• first");
        let numbered = render("1. first", 80);
        assert_eq!(text(&numbered[0]), "1. first");
    }

    #[test]
    fn snake_case_is_not_italicized() {
        let lines = render("call read_to_string(path)", 80);
        assert_eq!(text(&lines[0]), "call read_to_string(path)");
        assert!(
            !style_of(&lines[0], "read_to_string")
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn fenced_code_block_uses_background() {
        let lines = render("```rust\nlet x = 1;\n```", 80);
        assert!(lines.iter().any(|l| text(l).starts_with("let x = 1;")));
        let code = lines.iter().find(|l| text(l).contains("let x")).unwrap();
        assert_eq!(code.spans[0].style.bg, Some(CODE_BG));
    }

    #[test]
    fn long_lines_wrap_to_width() {
        let lines = render("word word word word word word", 11);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| text(l).chars().count() <= 11));
    }
}
