//! Painting: a frame is a grid of printable ASCII cells, each with one entry of the `Paint`
//! palette. Tests compare `Frame::text`; the terminal gets only the rows that changed.

use std::ffi::OsStr;
use std::io::Write;

/// How much colour the terminal takes. Text keeps the terminal's own foreground and ground
/// in every case; the brand's colours (brand/README.md) appear only as a fill with ink on it,
/// so they read on a light or a dark terminal alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Palette {
    /// `NO_COLOR`: attributes only.
    Mono,
    /// The terminal's own 16 colours: black on its blue, green and red stand in for the brand's.
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

/// A state in the brand's triad: blue is what is happening, green is what passed, pink is what
/// failed or needs a person. The word always goes with it; the colour never carries it alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Active,
    Ok,
    Fail,
}

// The brand's colours (brand/tokens.json) as SGR parameters. Macros, so `concat!` builds every
// escape below at compile time.
macro_rules! ink {
    () => {
        "15;15;15"
    };
}
macro_rules! blue {
    () => {
        "81;149;245"
    };
}
macro_rules! pink {
    () => {
        "238;54;106"
    };
}
macro_rules! green {
    () => {
        "54;238;168"
    };
}
macro_rules! grey {
    () => {
        "153;153;153"
    };
}
/// Ink text on one of the brand's colours.
macro_rules! on {
    ($fill:ident) => {
        concat!("\x1b[0;38;2;", ink!(), ";48;2;", $fill!(), "m")
    };
}
/// A solid cell of one of the brand's colours: its `#` drawn in the colour of its fill.
macro_rules! solid {
    ($fill:ident) => {
        concat!("\x1b[0;38;2;", $fill!(), ";48;2;", $fill!(), "m")
    };
}

/// A cell of a pixel worker: solid in its colour, grey while it waits, or one of its ink eyes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pixel {
    Pink,
    Green,
    Blue,
    Grey,
    Eye,
}

/// The whole palette. Colour appears only as a fill with ink on it: the status line, the state
/// chips and the pixel workers. Text on the terminal's own ground is never coloured, since no colour
/// chosen inside the program reads on every ground.
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
    /// The status line while it carries an error: ink on pink.
    Alert,
    /// An error's own words: bold. The `fail` chip before them carries the colour.
    Error,
    /// A state word on its tone's fill, in ink.
    Chip(Tone),
    /// A cell of a pixel worker.
    Pixel(Pixel),
}

impl Paint {
    /// What the cursor row paints a span in: the row turns reverse, but a chip and a pixel
    /// worker keep their fill, so a state still shows on the selected row.
    pub(crate) fn under_cursor(self) -> Paint {
        match self {
            Paint::Chip(_) | Paint::Pixel(_) => self,
            _ => Paint::Cursor,
        }
    }

    /// Under 16 colours a chip is black on the terminal's bright blue and red, since many
    /// themes keep the plain ones too dark for black text, and on its plain green, which some
    /// themes turn grey in the bright form; a pixel worker takes the same colours, and bright
    /// black for grey. Without colour a worker is its `#` drawing: bold at work, dim waiting.
    fn sgr(self, palette: Palette) -> &'static str {
        match (self, palette) {
            (Paint::Plain, _) => "\x1b[0m",
            (Paint::Muted, _) => "\x1b[0;2m",
            (Paint::Title, _) => "\x1b[0;1m",
            (Paint::Cursor, _) => "\x1b[0;7m",
            (Paint::Marked, _) => "\x1b[0;4m",
            (Paint::Status, Palette::True) => on!(blue),
            (Paint::Status, _) => "\x1b[0;7m",
            (Paint::Alert, Palette::True) => on!(pink),
            (Paint::Alert, Palette::Ansi) => "\x1b[0;30;101m",
            (Paint::Alert, Palette::Mono) => "\x1b[0;1;7m",
            (Paint::Error, _) => "\x1b[0;1m",
            (Paint::Chip(Tone::Active), Palette::True) => on!(blue),
            (Paint::Chip(Tone::Ok), Palette::True) => on!(green),
            (Paint::Chip(Tone::Fail), Palette::True) => on!(pink),
            (Paint::Chip(Tone::Active), Palette::Ansi) => "\x1b[0;30;104m",
            (Paint::Chip(Tone::Ok), Palette::Ansi) => "\x1b[0;30;42m",
            (Paint::Chip(Tone::Fail), Palette::Ansi) => "\x1b[0;30;101m",
            (Paint::Chip(Tone::Fail), Palette::Mono) => "\x1b[0;1m",
            (Paint::Chip(_), Palette::Mono) => "\x1b[0m",
            (Paint::Pixel(Pixel::Pink), Palette::True) => solid!(pink),
            (Paint::Pixel(Pixel::Green), Palette::True) => solid!(green),
            (Paint::Pixel(Pixel::Blue), Palette::True) => solid!(blue),
            (Paint::Pixel(Pixel::Grey), Palette::True) => solid!(grey),
            (Paint::Pixel(Pixel::Eye), Palette::True) => solid!(ink),
            (Paint::Pixel(Pixel::Pink), Palette::Ansi) => "\x1b[0;91;101m",
            (Paint::Pixel(Pixel::Green), Palette::Ansi) => "\x1b[0;32;42m",
            (Paint::Pixel(Pixel::Blue), Palette::Ansi) => "\x1b[0;94;104m",
            (Paint::Pixel(Pixel::Grey), Palette::Ansi) => "\x1b[0;90;100m",
            (Paint::Pixel(Pixel::Eye), Palette::Ansi) => "\x1b[0;30;40m",
            (Paint::Pixel(Pixel::Grey), Palette::Mono) => "\x1b[0;2m",
            (Paint::Pixel(Pixel::Eye), Palette::Mono) => "\x1b[0m",
            (Paint::Pixel(_), Palette::Mono) => "\x1b[0;1m",
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

    /// A state word as a chip: the word with a space either side, on the tone's fill. Placed
    /// where the plain row had a space either side, it keeps the row's text unchanged.
    pub(crate) fn chip(word: &str, tone: Tone) -> Span {
        Span::new(format!(" {word} "), Paint::Chip(tone))
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

    /// The paint of one cell.
    #[cfg(test)]
    pub(crate) fn paint_at(&self, row: usize, column: usize) -> Paint {
        self.cells[row][column].1
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

    const ALL: [Paint; 16] = [
        Paint::Plain,
        Paint::Muted,
        Paint::Title,
        Paint::Cursor,
        Paint::Marked,
        Paint::Status,
        Paint::Alert,
        Paint::Error,
        Paint::Chip(Tone::Active),
        Paint::Chip(Tone::Ok),
        Paint::Chip(Tone::Fail),
        Paint::Pixel(Pixel::Pink),
        Paint::Pixel(Pixel::Green),
        Paint::Pixel(Pixel::Blue),
        Paint::Pixel(Pixel::Grey),
        Paint::Pixel(Pixel::Eye),
    ];

    /// The SGR parameters of one paint.
    fn parameters(paint: Paint, palette: Palette) -> Vec<u16> {
        let sgr = paint.sgr(palette);
        let inner = sgr.strip_prefix("\x1b[").and_then(|s| s.strip_suffix('m'));
        let inner = inner.unwrap_or_else(|| panic!("{sgr:?}"));
        inner.split(';').map(|n| n.parse().unwrap()).collect()
    }

    /// The RGB foreground and ground of a truecolor paint.
    fn rgb(parameters: &[u16]) -> (Option<[u16; 3]>, Option<[u16; 3]>) {
        let (mut fg, mut bg) = (None, None);
        let mut at = 0;
        while at < parameters.len() {
            match parameters[at..] {
                [38, 2, r, g, b, ..] => (fg, at) = (Some([r, g, b]), at + 5),
                [48, 2, r, g, b, ..] => (bg, at) = (Some([r, g, b]), at + 5),
                _ => at += 1,
            }
        }
        (fg, bg)
    }

    #[test]
    fn colour_is_only_a_fill_with_ink_on_it() {
        let ink = [15, 15, 15];
        for paint in ALL {
            let mono = parameters(paint, Palette::Mono);
            assert!(
                mono.iter().all(|p| [0, 1, 2, 4, 7].contains(p)),
                "{paint:?}"
            );
            // 16 colours: a coloured foreground only on a fill, black or the fill's own colour
            // for a solid pixel; never a fill without its text colour.
            let ansi = parameters(paint, Palette::Ansi);
            let colour = |range: [u16; 2]| -> Vec<u16> {
                let ranges = [range[0]..=range[0] + 7, range[1]..=range[1] + 7];
                let within = |p: &&u16| ranges.iter().any(|r| r.contains(*p));
                ansi.iter().filter(within).copied().collect()
            };
            let (fg, bg) = (colour([30, 90]), colour([40, 100]));
            let attribute = |p: &u16| [0, 1, 2, 4, 7].contains(p);
            assert!(
                ansi.iter()
                    .all(|p| attribute(p) || fg.contains(p) || bg.contains(p))
            );
            assert_eq!(fg.len(), bg.len(), "{paint:?}");
            if let (Some(&fg), Some(&bg)) = (fg.first(), bg.first()) {
                assert!(fg == 30 || fg + 10 == bg, "{paint:?}");
            }
            // Truecolor: a coloured foreground only on a fill, in ink, or the fill's own colour
            // for a solid pixel.
            let (fg, bg) = rgb(&parameters(paint, Palette::True));
            if let Some(fg) = fg {
                assert!(bg.is_some_and(|bg| fg == ink || fg == bg), "{paint:?}");
            }
        }
    }

    #[test]
    fn every_colour_is_a_brand_token() {
        let tokens = include_str!("../../../../brand/tokens.json").to_ascii_lowercase();
        for paint in ALL {
            let (fg, bg) = rgb(&parameters(paint, Palette::True));
            for [r, g, b] in fg.into_iter().chain(bg) {
                let hex = format!("\"#{r:02x}{g:02x}{b:02x}\"");
                assert!(tokens.contains(&hex), "{paint:?} paints {hex}, not a token");
            }
        }
    }

    #[test]
    fn chips_keep_their_fill_on_the_cursor_row() {
        assert_eq!(Paint::Plain.under_cursor(), Paint::Cursor);
        assert_eq!(Paint::Error.under_cursor(), Paint::Cursor);
        let chip = Paint::Chip(Tone::Fail);
        assert_eq!(chip.under_cursor(), chip);
        assert_eq!(Span::chip("failed", Tone::Fail).text, " failed ");
    }

    #[test]
    fn the_status_line_chips_and_errors_in_each_palette() {
        let mut frame = Frame::new(14, 1);
        let spans = [
            Span::new("ab", Paint::Status),
            Span::chip("ok", Tone::Ok),
            Span::new("cd", Paint::Error),
            Span::new("ef", Paint::Alert),
        ];
        frame.paint_spans(0, 0, 14, &spans, Paint::Title);
        assert_eq!(frame.text(), "ab ok cdef\n");
        assert_eq!(
            frame.encode(0, Palette::True),
            "\x1b[0;38;2;15;15;15;48;2;81;149;245mab\x1b[0;38;2;15;15;15;48;2;54;238;168m ok \
             \x1b[0;1mcd\x1b[0;38;2;15;15;15;48;2;238;54;106mef\x1b[0;1m    \x1b[0m"
        );
        assert_eq!(
            frame.encode(0, Palette::Ansi),
            "\x1b[0;7mab\x1b[0;30;42m ok \x1b[0;1mcd\x1b[0;30;101mef\x1b[0;1m    \x1b[0m"
        );
        assert_eq!(
            frame.encode(0, Palette::Mono),
            "\x1b[0;7mab\x1b[0m ok \x1b[0;1mcd\x1b[0;1;7mef\x1b[0;1m    \x1b[0m"
        );
    }
}
