//! Painting: a frame is a grid of printable ASCII cells, each with one entry of the `Paint`
//! palette. Tests compare `Frame::text`; the terminal gets only the rows that changed.

use std::io::Write;

/// The whole palette. Colour is used for errors only, and `NO_COLOR` turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    Plain,
    Muted,
    Title,
    /// The cursor row of the focused region.
    Cursor,
    /// The cursor row of the region without focus.
    Marked,
    Status,
    Error,
}

impl Paint {
    fn sgr(self, color: bool) -> &'static str {
        match self {
            Paint::Plain => "\x1b[0m",
            Paint::Muted => "\x1b[0;2m",
            Paint::Title => "\x1b[0;1m",
            Paint::Cursor | Paint::Status => "\x1b[0;7m",
            Paint::Marked => "\x1b[0;4m",
            Paint::Error if color => "\x1b[0;1;31m",
            Paint::Error => "\x1b[0;1m",
        }
    }
}

/// A run of text in one paint. The text is already printable ASCII.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) text: String,
    pub(crate) paint: Paint,
}

impl Span {
    pub(crate) fn new(text: impl AsRef<str>, paint: Paint) -> Span {
        Span {
            text: ascii(text.as_ref()),
            paint,
        }
    }
}

/// Terminal text is printable ASCII only: a tab is a space and anything else is `?`, so no
/// byte a pane shows can move the cursor, change a mode, or impersonate another row.
pub(crate) fn ascii(text: &str) -> String {
    let mut printable = String::with_capacity(text.len());
    for character in text.chars() {
        printable.push(match character {
            ' '..='~' => character,
            '\t' => ' ',
            _ => '?',
        });
    }
    printable
}

/// `spans` without their first `skip` columns: the main pane's horizontal scroll.
pub(crate) fn scrolled(spans: &[Span], skip: usize) -> Vec<Span> {
    let mut skip = skip;
    let mut kept = Vec::new();
    for span in spans {
        if skip >= span.text.len() {
            skip -= span.text.len();
            continue;
        }
        kept.push(Span {
            text: span.text[skip..].to_owned(),
            paint: span.paint,
        });
        skip = 0;
    }
    kept
}

pub(crate) struct Frame {
    cells: Vec<Vec<(u8, Paint)>>,
}

impl Frame {
    pub(crate) fn new(width: usize, height: usize) -> Frame {
        Frame {
            cells: vec![vec![(b' ', Paint::Plain); width]; height],
        }
    }

    /// Paint `spans` into `row` from column `left`, clipped to `width` columns; the columns the
    /// spans do not reach take `fill`.
    pub(crate) fn paint_spans(
        &mut self,
        row: usize,
        left: usize,
        width: usize,
        spans: &[Span],
        fill: Paint,
    ) {
        let Some(cells) = self.cells.get_mut(row) else {
            return;
        };
        let end = left.saturating_add(width).min(cells.len());
        let start = left.min(end);
        for cell in &mut cells[start..end] {
            *cell = (b' ', fill);
        }
        let mut column = start;
        for span in spans {
            for byte in span.text.bytes() {
                if column >= end {
                    return;
                }
                cells[column] = (byte, span.paint);
                column += 1;
            }
        }
    }

    /// The frame as plain text: one line per row, trailing spaces trimmed.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut text = String::new();
        for cells in &self.cells {
            let line: String = cells.iter().map(|(byte, _)| char::from(*byte)).collect();
            text.push_str(line.trim_end());
            text.push('\n');
        }
        text
    }

    fn encode(&self, row: usize, color: bool) -> String {
        let mut encoded = String::new();
        let mut current = None;
        for (byte, paint) in &self.cells[row] {
            if current != Some(*paint) {
                encoded.push_str(paint.sgr(color));
                current = Some(*paint);
            }
            encoded.push(char::from(*byte));
        }
        encoded.push_str(Paint::Plain.sgr(color));
        encoded
    }
}

/// Write the rows of `frame` that differ from `shown`, which holds what the terminal shows now,
/// and remember them. An empty `shown` repaints everything.
pub(crate) fn paint(
    frame: &Frame,
    shown: &mut Vec<String>,
    out: &mut impl Write,
    color: bool,
) -> std::io::Result<()> {
    shown.resize(frame.cells.len(), String::new());
    let mut bytes = Vec::new();
    for (row, previous) in shown.iter_mut().enumerate() {
        let encoded = frame.encode(row, color);
        if *previous != encoded {
            write!(bytes, "\x1b[{};1H{encoded}", row + 1)?;
            *previous = encoded;
        }
    }
    if !bytes.is_empty() {
        out.write_all(&bytes)?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_data_is_printable_ascii() {
        let hostile = "ok\u{1b}[2J\n\u{202e}\tend";
        assert_eq!(ascii(hostile), "ok?[2J?? end");
        let span = Span::new(hostile, Paint::Plain);
        let printable = span.text.bytes().all(|b| (0x20..=0x7e).contains(&b));
        assert!(printable);
    }

    #[test]
    fn spans_clip_to_their_region_and_scroll() {
        let mut frame = Frame::new(12, 2);
        let title = Span::new("abc", Paint::Title);
        let spans = [title, Span::new("defghij", Paint::Plain)];
        frame.paint_spans(0, 2, 6, &spans, Paint::Plain);
        frame.paint_spans(1, 0, 12, &scrolled(&spans, 4), Paint::Cursor);
        assert_eq!(frame.text(), "  abcdef\nefghij\n");
        let mut shown = Vec::new();
        let mut out = Vec::new();
        paint(&frame, &mut shown, &mut out, false).unwrap();
        let written = String::from_utf8(out).unwrap();
        let first = "\x1b[1;1H\x1b[0m  \x1b[0;1mabc\x1b[0mdef";
        let second = "\x1b[2;1H\x1b[0mefghij\x1b[0;7m      \x1b[0m";
        assert!(written.starts_with(first), "{written:?}");
        assert!(written.contains(second), "{written:?}");
        // Nothing changed, so nothing is written again.
        let mut again = Vec::new();
        paint(&frame, &mut shown, &mut again, false).unwrap();
        assert!(again.is_empty());
    }
}
