//! Painting: a frame is a grid of printable ASCII cells, each with one entry of the `Paint`
//! palette. Tests compare `Frame::text`; the terminal gets only the rows that changed.

use std::ffi::OsStr;
use std::io::Write;

/// How much colour the terminal takes. Text keeps the terminal's own foreground and ground
/// in every case; the brand's colours (brand/README.md) appear only where the frame paints
/// both, so they read on a light or a dark terminal alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Palette {
    /// `NO_COLOR`: attributes only.
    Mono,
    /// The terminal's own 16 colours: its red stands in for the brand's pink.
    Ansi,
    /// `COLORTERM=truecolor`: the brand's values.
    True,
}

impl Palette {
    pub(crate) fn from_env() -> Palette {
        Palette::detect(
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("COLORTERM").as_deref(),
        )
    }

    /// `NO_COLOR` set and non-empty wins; then `COLORTERM` names truecolor.
    pub(crate) fn detect(no_color: Option<&OsStr>, colorterm: Option<&OsStr>) -> Palette {
        if no_color.is_some_and(|value| !value.is_empty()) {
            return Palette::Mono;
        }
        match colorterm.and_then(OsStr::to_str) {
            Some("truecolor") | Some("24bit") => Palette::True,
            _ => Palette::Ansi,
        }
    }
}

/// The whole palette. Colour is used for the status line and for errors only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    Plain,
    Muted,
    Title,
    /// The cursor row of the focused region.
    Cursor,
    /// The cursor row of the region without focus.
    Marked,
    /// The status line: ink on the brand's blue where the terminal takes it, else reverse.
    Status,
    Error,
}

impl Paint {
    fn sgr(self, palette: Palette) -> &'static str {
        match (self, palette) {
            (Paint::Plain, _) => "\x1b[0m",
            (Paint::Muted, _) => "\x1b[0;2m",
            (Paint::Title, _) => "\x1b[0;1m",
            (Paint::Cursor, _) => "\x1b[0;7m",
            (Paint::Marked, _) => "\x1b[0;4m",
            (Paint::Status, Palette::True) => "\x1b[0;38;2;15;15;15;48;2;81;149;245m",
            (Paint::Status, _) => "\x1b[0;7m",
            (Paint::Error, Palette::Mono) => "\x1b[0;1m",
            (Paint::Error, Palette::Ansi) => "\x1b[0;1;31m",
            (Paint::Error, Palette::True) => "\x1b[0;1;38;2;238;54;106m",
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

    fn encode(&self, row: usize, palette: Palette) -> String {
        let mut encoded = String::new();
        let mut current = None;
        for (byte, paint) in &self.cells[row] {
            if current != Some(*paint) {
                encoded.push_str(paint.sgr(palette));
                current = Some(*paint);
            }
            encoded.push(char::from(*byte));
        }
        encoded.push_str(Paint::Plain.sgr(palette));
        encoded
    }
}

/// Write the rows of `frame` that differ from `shown`, which holds what the terminal shows now,
/// and remember them. An empty `shown` repaints everything.
pub(crate) fn paint(
    frame: &Frame,
    shown: &mut Vec<String>,
    out: &mut impl Write,
    palette: Palette,
) -> std::io::Result<()> {
    shown.resize(frame.cells.len(), String::new());
    let mut bytes = Vec::new();
    for (row, previous) in shown.iter_mut().enumerate() {
        let encoded = frame.encode(row, palette);
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
        paint(&frame, &mut shown, &mut out, Palette::Mono).unwrap();
        let written = String::from_utf8(out).unwrap();
        let first = "\x1b[1;1H\x1b[0m  \x1b[0;1mabc\x1b[0mdef";
        let second = "\x1b[2;1H\x1b[0mefghij\x1b[0;7m      \x1b[0m";
        assert!(written.starts_with(first), "{written:?}");
        assert!(written.contains(second), "{written:?}");
        // Nothing changed, so nothing is written again.
        let mut again = Vec::new();
        paint(&frame, &mut shown, &mut again, Palette::Mono).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn palette_follows_no_color_then_colorterm() {
        let os = |value: &str| Some(OsStr::new(value)).map(|s| s.to_owned());
        let detect = |no_color: Option<std::ffi::OsString>,
                      colorterm: Option<std::ffi::OsString>| {
            Palette::detect(no_color.as_deref(), colorterm.as_deref())
        };
        assert_eq!(detect(os("1"), os("truecolor")), Palette::Mono);
        assert_eq!(detect(os(""), os("truecolor")), Palette::True);
        assert_eq!(detect(None, os("24bit")), Palette::True);
        assert_eq!(detect(None, os("yes")), Palette::Ansi);
        assert_eq!(detect(None, None), Palette::Ansi);
    }

    #[test]
    fn brand_colours_paint_only_the_status_line_and_errors() {
        let mut frame = Frame::new(8, 1);
        let spans = [
            Span::new("ab", Paint::Status),
            Span::new("cd", Paint::Error),
        ];
        frame.paint_spans(0, 0, 8, &spans, Paint::Title);
        assert_eq!(
            frame.encode(0, Palette::True),
            "\x1b[0;38;2;15;15;15;48;2;81;149;245mab\x1b[0;1;38;2;238;54;106mcd\x1b[0;1m    \x1b[0m"
        );
        assert_eq!(
            frame.encode(0, Palette::Ansi),
            "\x1b[0;7mab\x1b[0;1;31mcd\x1b[0;1m    \x1b[0m"
        );
        let mono = frame.encode(0, Palette::Mono);
        assert_eq!(mono, "\x1b[0;7mab\x1b[0;1mcd\x1b[0;1m    \x1b[0m");
        assert!(!mono.contains(";3") && !mono.contains(";4"), "{mono:?}");
    }
}
