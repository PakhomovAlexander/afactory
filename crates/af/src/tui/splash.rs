//! The splash bare `af` paints the moment the terminal is entered, while the scope and every
//! pane load behind it: the three workers in a row, pink, green and blue, on a conveyor belt. A
//! Task block rides the belt; the worker it reaches is at work, in its colour, and the other two
//! wait in grey. A worker moves only by that change of state (brand/README.md). The browser
//! replaces the splash as soon as it has loaded, so the splash never makes `af` slower.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::Host as _;
use super::paint::{Frame, Paint, Palette, Pixel, Span};
use super::term::Session;

/// The worker of `brand/ascii.txt`, 12 columns by 6 rows; `o` marks an eye, a space painted ink.
const WORKER: [&str; 6] = [
    "     ###    ",
    "  ######### ",
    "  #oo###oo# ",
    "  #oo###oo# ",
    "  ######### ",
    " ###########",
];
const WORKER_WIDTH: usize = 12;
/// Columns between two workers.
const GAP: usize = 3;
/// The row of three workers, and the belt under it.
const WIDTH: usize = 3 * WORKER_WIDTH + 2 * GAP;
/// The workers, the belt, a blank row and the name.
const HEIGHT: usize = WORKER.len() + 3;
/// The belt's pattern; it moves one column a step, left to right.
const BELT: &[u8] = b"=--";
const TASK: &str = "[#]";
/// One step of the animation: the belt and the Task move one column.
const STEP: Duration = Duration::from_millis(60);
const TAGLINE: &str = "agent pipelines made fast";

/// One row of a worker in `pixel`'s colour, its eyes ink.
pub(crate) fn worker_spans(row: usize, pixel: Pixel) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for cell in WORKER[row].chars() {
        let (text, paint) = match cell {
            '#' => ('#', Paint::Pixel(pixel)),
            'o' => (' ', Paint::Pixel(Pixel::Eye)),
            _ => (' ', Paint::Plain),
        };
        match spans.last_mut() {
            Some(span) if span.paint == paint => span.text.push(text),
            _ => spans.push(Span::new(text.to_string(), paint)),
        }
    }
    spans
}

/// Where the Task block is on the belt at `step`, and which worker it has reached.
fn task_at(step: usize) -> (usize, usize) {
    let column = step % (WIDTH - TASK.len() + 1);
    let worker = ((column + 1) / (WORKER_WIDTH + GAP)).min(2);
    (column, worker)
}

/// The splash at `step` on a `width` x `height` screen, centred; clipped on a smaller one.
pub(crate) fn frame(width: usize, height: usize, step: usize) -> Frame {
    let mut frame = Frame::new(width, height);
    let left = width.saturating_sub(WIDTH) / 2;
    let top = height.saturating_sub(HEIGHT) / 2;
    let (column, working) = task_at(step);
    for row in 0..WORKER.len() {
        for (index, pixel) in [Pixel::Pink, Pixel::Green, Pixel::Blue]
            .into_iter()
            .enumerate()
        {
            let pixel = if index == working { pixel } else { Pixel::Grey };
            let at = left + index * (WORKER_WIDTH + GAP);
            let spans = worker_spans(row, pixel);
            frame.paint_spans(top + row, at, WORKER_WIDTH, &spans, Paint::Plain);
        }
    }
    let belt: String = (0..WIDTH)
        .map(|at| char::from(BELT[(at + BELT.len() - step % BELT.len()) % BELT.len()]))
        .collect();
    let belt = [
        Span::new(&belt[..column], Paint::Muted),
        Span::new(TASK, Paint::Title),
        Span::new(&belt[column + TASK.len()..], Paint::Muted),
    ];
    frame.paint_spans(top + WORKER.len(), left, WIDTH, &belt, Paint::Plain);
    let name = format!("af {}", env!("CARGO_PKG_VERSION"));
    let line = [
        Span::new(&name, Paint::Title),
        Span::new(format!("   {TAGLINE}"), Paint::Muted),
    ];
    let length = name.len() + 3 + TAGLINE.len();
    let at = width.saturating_sub(length) / 2;
    frame.paint_spans(top + HEIGHT - 1, at, length, &line, Paint::Plain);
    frame
}

/// How the splash ended.
pub(crate) enum Ended<T> {
    /// The browser loaded; the keys typed meanwhile are the browser's.
    Loaded(T, Vec<u8>),
    /// `q` or `<C-c>` while it loaded.
    Quit,
}

/// Whether keys typed during the splash ask to quit: `q`, or `<C-c>`, which raw mode reads as
/// a byte.
fn quits(keys: &[u8]) -> bool {
    keys.iter().any(|key| matches!(key, b'q' | 0x03))
}

/// Animate the splash until `loaded` delivers or the user quits. The load is the clock: the
/// splash waits on it a step at a time and ends the moment it arrives.
pub(crate) fn show<T>(
    session: &mut Session,
    palette: Palette,
    loaded: &Receiver<T>,
) -> Result<Ended<T>, String> {
    let start = Instant::now();
    let mut shown = Vec::new();
    let mut size = (0, 0);
    let mut typed = Vec::new();
    let mut buffer = [0_u8; 512];
    loop {
        let now = session.size();
        if now != size {
            size = now;
            shown.clear();
            session.send(b"\x1b[2J")?;
        }
        let step = (start.elapsed().as_millis() / STEP.as_millis()) as usize;
        session.paint(&frame(size.0, size.1, step), &mut shown, palette)?;
        match loaded.recv_timeout(STEP) {
            Ok(value) => return Ok(Ended::Loaded(value, typed)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err("loading the browser stopped before it finished".to_owned());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let count = session.read_ready(&mut buffer)?;
        if quits(&buffer[..count]) {
            return Ok(Ended::Quit);
        }
        typed.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paint of each worker's chimney top, left to right.
    fn workers(frame: &Frame, top: usize, left: usize) -> Vec<Paint> {
        (0..3)
            .map(|index| frame.paint_at(top, left + index * (WORKER_WIDTH + GAP) + 5))
            .collect()
    }

    #[test]
    fn the_splash_is_three_workers_on_a_belt_above_the_name() {
        let frame = frame(80, 24, 0);
        let text = frame.text();
        let lines: Vec<&str> = text.lines().collect();
        let (top, left) = ((24 - HEIGHT) / 2, (80 - WIDTH) / 2);
        let pad = " ".repeat(left);
        assert_eq!(
            lines[top..top + HEIGHT],
            [
                format!("{pad}     ###            ###            ###"),
                format!("{pad}  #########      #########      #########"),
                format!("{pad}  #  ###  #      #  ###  #      #  ###  #"),
                format!("{pad}  #  ###  #      #  ###  #      #  ###  #"),
                format!("{pad}  #########      #########      #########"),
                format!("{pad} ###########    ###########    ###########"),
                format!("{pad}[#]=--=--=--=--=--=--=--=--=--=--=--=--=--"),
                String::new(),
                format!(
                    "{}af {}   {TAGLINE}",
                    " ".repeat((80 - 6 - TAGLINE.len() - env!("CARGO_PKG_VERSION").len()) / 2),
                    env!("CARGO_PKG_VERSION")
                ),
            ]
        );
        // The pink worker has the Task; the other two wait in grey, every eye ink.
        let grey = Paint::Pixel(Pixel::Grey);
        assert_eq!(
            workers(&frame, top, left),
            [Paint::Pixel(Pixel::Pink), grey, grey]
        );
        assert_eq!(frame.paint_at(top + 2, left + 3), Paint::Pixel(Pixel::Eye));
        assert_eq!(frame.paint_at(top + 6, left), Paint::Title);
        assert_eq!(frame.paint_at(top + 6, left + 3), Paint::Muted);
    }

    #[test]
    fn the_task_rides_the_belt_and_lights_the_worker_it_reaches() {
        let (top, left) = ((24 - HEIGHT) / 2, (80 - WIDTH) / 2);
        let grey = Paint::Pixel(Pixel::Grey);
        let green = Paint::Pixel(Pixel::Green);
        let blue = Paint::Pixel(Pixel::Blue);
        let at = |step| frame(80, 24, step);
        assert_eq!(workers(&at(20), top, left), [grey, green, grey]);
        assert_eq!(workers(&at(36), top, left), [grey, grey, blue]);
        // The belt and the Task move one column to the right a step, and the Task starts over.
        let belt = |step: usize| at(step).text().lines().nth(top + 6).unwrap().to_owned();
        assert_eq!(&belt(1)[left..left + 6], "-[#]=-");
        assert_eq!(&belt(2)[left..left + 7], "--[#]=-");
        assert_eq!(task_at(WIDTH - TASK.len()), (WIDTH - TASK.len(), 2));
        assert_eq!(&belt(WIDTH - TASK.len() + 1)[left..left + 3], TASK);
    }

    #[test]
    fn a_small_screen_clips_the_splash() {
        let frame = frame(20, 4, 7);
        assert_eq!(frame.text().lines().count(), 4);
    }

    #[test]
    fn q_and_ctrl_c_quit_the_splash_and_other_keys_wait_for_the_browser() {
        assert!(quits(b"q"));
        assert!(quits(b"jj\x03"));
        assert!(!quits(b"]]j"));
        assert!(!quits(b""));
    }
}
