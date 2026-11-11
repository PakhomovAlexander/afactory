//! Parse and validation errors quote at most a short excerpt of the offending source line.
//!
//! A TOML error echoes the line it failed on with a caret row below it. Authority files come
//! from repositories af does not control, so one hostile line of hundreds of kilobytes must not
//! become hundreds of kilobytes of stderr: every quoted line keeps at most
//! [`MAX_EXCERPT_BYTES`] and ends in an ellipsis when cut. A caret within the excerpt stays
//! aligned under its column; one beyond it is dropped, and the error's own `line, column`
//! header still names the place.

/// The most bytes of one source line (or any one message line) an error quotes.
pub const MAX_EXCERPT_BYTES: usize = 200;

const ELLIPSIS: &str = "…";

/// `message` with every line longer than [`MAX_EXCERPT_BYTES`] cut to an excerpt. A line in the
/// `N | source` gutter form keeps its gutter; a caret row beneath one stays aligned when its
/// column is inside the excerpt and is dropped when it is not.
pub fn bounded(message: &str) -> String {
    let mut lines = Vec::new();
    // Whether the source line just above was cut, and where.
    let mut cut_above: Option<usize> = None;
    for line in message.lines() {
        match gutter(line) {
            Some((prefix, body)) if is_caret_row(body) => {
                let column = body.len() - body.trim_start().len();
                match cut_above {
                    Some(_) if column >= MAX_EXCERPT_BYTES => {}
                    Some(kept) => lines.push(format!("{prefix}{}", cut(body, kept))),
                    None => lines.push(format!("{prefix}{}", cut(body, MAX_EXCERPT_BYTES))),
                }
                cut_above = None;
            }
            Some((prefix, body)) => {
                cut_above = (body.len() > MAX_EXCERPT_BYTES).then_some(MAX_EXCERPT_BYTES);
                lines.push(format!("{prefix}{}", cut(body, MAX_EXCERPT_BYTES)));
            }
            None => {
                cut_above = None;
                lines.push(cut(line, MAX_EXCERPT_BYTES));
            }
        }
    }
    let mut bounded = lines.join("\n");
    if message.ends_with('\n') {
        bounded.push('\n');
    }
    bounded
}

/// `text` as an error may quote it: at most [`MAX_EXCERPT_BYTES`], cut with an ellipsis.
pub fn quoted(text: &str) -> String {
    cut(text, MAX_EXCERPT_BYTES)
}

/// `line` split after a `  N | ` or `  | ` gutter: spaces, optional digits, spaces, a bar.
fn gutter(line: &str) -> Option<(&str, &str)> {
    let bar = line.find('|')?;
    let head = &line[..bar];
    if !head
        .bytes()
        .all(|byte| byte == b' ' || byte.is_ascii_digit())
    {
        return None;
    }
    let body_start = if line[bar + 1..].starts_with(' ') {
        bar + 2
    } else {
        bar + 1
    };
    Some((&line[..body_start], &line[body_start..]))
}

fn is_caret_row(body: &str) -> bool {
    let marks = body.trim_start();
    !marks.is_empty() && marks.bytes().all(|byte| matches!(byte, b'^' | b'-' | b' '))
}

/// `text` within `limit` bytes, at a character boundary, with an ellipsis when cut.
fn cut(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELLIPSIS}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_error_is_unchanged() {
        let error = toml::from_str::<toml::Value>("x = [1,\n").unwrap_err();
        let text = error.to_string();
        assert_eq!(bounded(&text), text);
    }

    #[test]
    fn a_huge_offending_line_is_quoted_as_a_short_excerpt() {
        let source = format!("x = {}", "[".repeat(200_000));
        let error = toml::from_str::<toml::Value>(&source).unwrap_err();
        let shown = bounded(&error.to_string());
        assert!(shown.len() < 1024, "{} bytes: {shown}", shown.len());
        assert!(shown.contains(ELLIPSIS), "{shown}");
        assert!(
            shown
                .lines()
                .all(|line| line.len() <= 2 * MAX_EXCERPT_BYTES)
        );
    }

    #[test]
    fn a_caret_within_the_excerpt_stays_aligned_and_one_beyond_it_is_dropped() {
        let long = "a".repeat(400);
        let near = format!("error at line 1, column 5\n  |\n1 | {long}\n  |     ^\nexpected\n");
        let shown = bounded(&near);
        let lines: Vec<&str> = shown.lines().collect();
        assert_eq!(
            lines[2],
            format!("1 | {}{ELLIPSIS}", "a".repeat(MAX_EXCERPT_BYTES))
        );
        assert_eq!(lines[3], "  |     ^");
        assert_eq!(lines[4], "expected");
        let far = format!(
            "error at line 1, column 300\n  |\n1 | {long}\n  | {}^\nexpected\n",
            " ".repeat(300)
        );
        let shown = bounded(&far);
        assert!(!shown.contains('^'), "{shown}");
        assert!(shown.contains("column 300"));
        assert!(shown.ends_with("expected\n"));
    }

    #[test]
    fn a_quoted_value_is_cut_to_the_excerpt() {
        assert_eq!(quoted("short"), "short");
        let long = "p".repeat(1024);
        assert_eq!(
            quoted(&long),
            format!("{}{ELLIPSIS}", "p".repeat(MAX_EXCERPT_BYTES))
        );
    }

    #[test]
    fn an_excerpt_is_cut_at_a_character_boundary() {
        let line = "é".repeat(MAX_EXCERPT_BYTES);
        let shown = bounded(&line);
        assert!(shown.len() <= MAX_EXCERPT_BYTES + ELLIPSIS.len());
        assert!(shown.ends_with(ELLIPSIS));
    }
}
