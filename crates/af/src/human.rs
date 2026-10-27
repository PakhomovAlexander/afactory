//! Figures as a person reads them: durations and sizes in units, times in the local zone, and
//! lines held to the terminal's width with commands kept whole.
//!
//! Human text uses these; the raw figure (milliseconds, bytes, Unix time) is printed only under
//! `--json`, where a script reads it.

use std::path::Path;

/// A duration in its two largest units: `850 ms`, `4.2 s`, `3 min 05 s`, `2 h 59 min`,
/// `3 d 4 h`. Truncated, never rounded up.
pub(crate) fn duration(ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..MINUTE => format!("{}.{} s", ms / 1_000, ms % 1_000 / 100),
        MINUTE..HOUR => format!("{} min {:02} s", ms / MINUTE, ms % MINUTE / 1_000),
        HOUR..DAY => format!("{} h {:02} min", ms / HOUR, ms % HOUR / MINUTE),
        _ => format!("{} d {} h", ms / DAY, ms % DAY / HOUR),
    }
}

/// A size in binary units with one decimal: `512 B`, `1.2 GiB`.
pub(crate) fn bytes(bytes: u64) -> String {
    crate::storage::human_bytes(bytes)
}

/// The widest line human text on stdout may print: the terminal's width when stdout is one,
/// else `COLUMNS` as clap reads it for `--help`, else 100; never more than 100, the width help
/// pages are held to.
pub(crate) fn width() -> usize {
    let columns = || {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
    };
    held(terminal_columns(std::io::stdout()).or_else(columns))
}

/// The columns of the terminal `stream` is, when it is one and says.
fn terminal_columns<S: std::io::IsTerminal + rustix::fd::AsFd>(stream: S) -> Option<usize> {
    stream
        .is_terminal()
        .then(|| rustix::termios::tcgetwinsize(&stream).ok())
        .flatten()
        .map(|size| usize::from(size.ws_col))
        .filter(|&columns| columns > 0)
}

/// `columns` held to the widths human text is laid out for: 40 to 100, 100 when unknown.
fn held(columns: Option<usize>) -> usize {
    match columns {
        Some(columns) if columns >= 40 => columns.min(100),
        Some(columns) if columns > 0 => 40,
        _ => 100,
    }
}

/// `line`, ending in a command (`next: af task run t --state DIR`), as a terminal shows it: on
/// stdout, or stderr when `stderr`, that is a terminal, fitted to its own width (at least 40
/// columns, never held to 100) by [`fit_command`];
/// unchanged on a stream that is no terminal, whose reader takes the line whole (a script reads
/// the run log's last line, `af help agents`).
pub(crate) fn fit_on_terminal(line: &str, stderr: bool) -> String {
    let columns = if stderr {
        terminal_columns(std::io::stderr())
    } else {
        terminal_columns(std::io::stdout())
    };
    // The terminal's own width, however wide: a line that fits it is never broken.
    match columns {
        Some(columns) => fit_command(line, columns.max(40)),
        None => line.to_owned(),
    }
}

/// `text` wrapped at spaces to lines of at most `width` columns, every line after the first
/// indented by `indent` spaces; a line that fits is returned as it is, its spacing kept. A span
/// between backticks is one word, so a command named in prose is never split across lines; a
/// word wider than a line keeps a line of its own.
pub(crate) fn wrap(text: &str, width: usize, indent: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    // Each word with the spaces before it, so the first line keeps its own indent and spacing.
    let mut segments: Vec<(String, String)> = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        let mut spaces = String::new();
        while let Some(c) = chars.next_if_eq(&' ') {
            spaces.push(c);
        }
        let mut word = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == ' ' && !quoted {
                break;
            }
            quoted ^= c == '`';
            word.push(c);
            chars.next();
        }
        if word.is_empty() {
            break;
        }
        segments.push((spaces, word));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (spaces, word) in segments {
        let wide = line.chars().count() + spaces.chars().count() + word.chars().count() > width;
        if wide && !line.trim().is_empty() {
            lines.push(std::mem::take(&mut line));
            line = format!("{}{word}", " ".repeat(indent));
        } else {
            line.push_str(&spaces);
            line.push_str(&word);
        }
    }
    lines.push(line);
    lines.join("\n")
}

/// `line`, a label and an `af` command (`next:    af review run --policy-rev HEAD ...`), kept
/// within `width` columns: unchanged when it fits, else continued with ` \` between the
/// command's arguments, each continuation indented four columns past the command's start (four
/// columns in all where that start is past half the width), so a copied command runs as
/// printed. An argument wider than any line, such as a long `--state` path or the program's own
/// path, is cut into quoted pieces joined by a backslash and a newline, which a shell removes,
/// so the pieces read as the one argument; each piece after the first starts its line. A label
/// too wide to leave the command room beside it (`Task ID: STATE; next:` with a long Task ID)
/// is wrapped at its spaces first, a word wider than a line cut at the width. A line without an
/// `af` command, or one whose quoting a shell would not read, is returned unchanged.
pub(crate) fn fit_command(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let start = if line.starts_with("af ") {
        Some(0)
    } else {
        line.find(" af ").map(|at| at + 1)
    };
    let Some(start) = start else {
        return line.to_owned();
    };
    fit_at(line, start, width)
}

/// The columns a command keeps beside its label: a label wider than `width` less these is
/// wrapped onto lines of its own.
const COMMAND_ROOM: usize = 16;

/// [`fit_command`] for a line whose command starts at byte `start`, whatever its program: the
/// path of an `af` binary, say. Its words must be plain arguments, no shell operator. No line it
/// returns is wider than `width` (at least 40): not the label, not the first word, not a
/// continuation.
pub(crate) fn fit_at(line: &str, start: usize, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let Ok(words) = shell_words::split(&line[start..]) else {
        return line.to_owned();
    };
    fit_words(&line[..start], &words, width, &|word| {
        shell_words::quote(word).into_owned()
    })
}

/// [`fit_at`] for a command already split into its `words`, each written by `quote` (sh's
/// quoting, or fish's for a program path fish reads): after `label`, unchanged when it fits.
pub(crate) fn fit_words(
    label: &str,
    words: &[String],
    width: usize,
    quote: &dyn Fn(&str) -> String,
) -> String {
    let count = |text: &str| text.chars().count();
    let line = format!(
        "{label}{}",
        words
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if count(&line) <= width {
        return line;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = if count(label) + COMMAND_ROOM <= width {
        label.to_owned()
    } else {
        // The label's own lines, its last one kept for the command when there is room on it.
        let mut rows = cut_rows(&wrap(label.trim_end(), width, 0), width);
        let gap = &label[label.trim_end().len()..];
        let last = rows.pop().unwrap_or_default();
        lines.extend(rows);
        if count(&last) + count(gap) + COMMAND_ROOM <= width {
            format!("{last}{gap}")
        } else {
            lines.push(last);
            String::new()
        }
    };
    let indent = if count(&current) + 4 <= width / 2 {
        " ".repeat(count(&current) + 4)
    } else {
        " ".repeat(4)
    };
    let margin = indent.chars().count();
    // Whether `current` holds no word of the command yet.
    let mut fresh = true;
    for raw in words {
        let word = quote(raw);
        let space = usize::from(!fresh);
        if count(&current) + space + count(&word) + 2 <= width {
            if !fresh {
                current.push(' ');
            }
            current.push_str(&word);
            fresh = false;
            continue;
        }
        if !fresh {
            lines.push(format!("{current} \\"));
            current = indent.clone();
            if margin + count(&word) + 2 <= width {
                current.push_str(&word);
                continue;
            }
        }
        // Wider than any line, the program's path included: in pieces, the first after what
        // the line holds, every other at the start of its own line.
        let mut pieces = pieces(
            raw,
            width.saturating_sub(count(&current) + 2),
            width.saturating_sub(2),
            quote,
        );
        let last = pieces.pop().unwrap_or_default();
        for piece in pieces {
            lines.push(format!("{current}{piece}\\"));
            current.clear();
        }
        current.push_str(&last);
        fresh = false;
    }
    lines.push(current);
    lines.join("\n")
}

/// Each line of `text` within `width` columns: a line wider than that, one word too long for
/// any line, is cut at the width.
fn cut_rows(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        for chunk in chars.chunks(width.max(1)) {
            rows.push(chunk.iter().collect());
        }
    }
    rows
}

/// `word` as pieces, each quoted by `quote`, whose concatenation a shell reads as `word`: the
/// first at most `first` columns, every other at most `rest`, each cut after a `/` in its second
/// half where it has one, so a path breaks between its directories.
pub(crate) fn pieces(
    word: &str,
    first: usize,
    rest: usize,
    quote: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let quoted = |chars: &[char]| quote(&chars.iter().collect::<String>());
    let chars: Vec<char> = word.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let room = if pieces.is_empty() { first } else { rest }.max(8);
        let mut end = start + 1;
        while end < chars.len() && quoted(&chars[start..=end]).chars().count() <= room {
            end += 1;
        }
        if end < chars.len()
            && let Some(slash) = chars[start..end].iter().rposition(|c| *c == '/')
            && slash + 1 > (end - start) / 2
        {
            end = start + slash + 1;
        }
        pieces.push(quoted(&chars[start..end]));
        start = end;
    }
    pieces
}

/// The UTC offset in seconds and the zone's abbreviation at `unix_seconds`, from the zone
/// `TZ` names or the system's `/etc/localtime`; UTC when neither can be read.
fn zone_at(unix_seconds: i64) -> (i64, String) {
    let file = match std::env::var("TZ") {
        Ok(name) if name.is_empty() || name == "UTC" || name == "UTC0" || name == ":UTC" => {
            return (0, "UTC".into());
        }
        Ok(name) => {
            let name = name.trim_start_matches(':');
            if name.starts_with('/') {
                std::path::PathBuf::from(name)
            } else if name.split('/').all(|part| {
                !part.is_empty()
                    && part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'))
            }) {
                Path::new("/usr/share/zoneinfo").join(name)
            } else {
                return (0, "UTC".into());
            }
        }
        Err(_) => std::path::PathBuf::from("/etc/localtime"),
    };
    std::fs::read(file)
        .ok()
        .and_then(|data| tzif_zone(&data, unix_seconds))
        .unwrap_or((0, "UTC".into()))
}

/// The offset and abbreviation a TZif file gives at `at`: its version 2+ 64-bit block when it
/// has one, else its version 1 block, the transition at or before `at`, else its first type.
fn tzif_zone(data: &[u8], at: i64) -> Option<(i64, String)> {
    fn counts(header: &[u8]) -> Option<[usize; 6]> {
        if header.get(..4)? != b"TZif" {
            return None;
        }
        let mut counts = [0usize; 6];
        for (index, count) in counts.iter_mut().enumerate() {
            let at = 20 + index * 4;
            *count = u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?) as usize;
        }
        Some(counts)
    }
    let [isutc, isstd, leap, transitions, types, chars] = counts(data)?;
    let v1_length = transitions * 5 + types * 6 + chars + leap * 8 + isstd + isutc;
    let (block, time_size) = if data.get(4).copied().unwrap_or(0) >= b'2' {
        (data.get(44 + v1_length..)?, 8)
    } else {
        (data, 4)
    };
    let [_, _, _, transitions, types, chars] = counts(block)?;
    let body = block.get(44..)?;
    let times = body.get(..transitions * time_size)?;
    let indices = body.get(transitions * time_size..transitions * (time_size + 1))?;
    let infos =
        body.get(transitions * (time_size + 1)..transitions * (time_size + 1) + types * 6)?;
    let abbreviations = body.get(
        transitions * (time_size + 1) + types * 6
            ..transitions * (time_size + 1) + types * 6 + chars,
    )?;
    let time = |index: usize| -> Option<i64> {
        let raw = times.get(index * time_size..(index + 1) * time_size)?;
        Some(if time_size == 8 {
            i64::from_be_bytes(raw.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(raw.try_into().ok()?))
        })
    };
    let mut type_index = 0usize;
    for index in 0..transitions {
        if time(index)? <= at {
            type_index = usize::from(*indices.get(index)?);
        } else {
            break;
        }
    }
    let info = infos.get(type_index * 6..type_index * 6 + 6)?;
    let offset = i64::from(i32::from_be_bytes(info.get(..4)?.try_into().ok()?));
    let start = usize::from(info[5]);
    let end = abbreviations
        .get(start..)?
        .iter()
        .position(|b| *b == 0)
        .map_or(abbreviations.len(), |length| start + length);
    let name = String::from_utf8_lossy(abbreviations.get(start..end)?).into_owned();
    let name = if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "UTC{sign}{:02}:{:02}",
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        )
    } else {
        name
    };
    Some((offset, name))
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A moment as local time with its zone, to the minute: `2026-10-23 06:12 CEST`.
pub(crate) fn local_time(unix_ms: u64) -> String {
    let seconds = i64::try_from(unix_ms / 1_000).unwrap_or(i64::MAX / 2);
    let (offset, zone) = zone_at(seconds);
    let local = seconds + offset;
    let (year, month, day) = civil(local.div_euclid(86_400));
    let of_day = local.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} {zone}",
        of_day / 3_600,
        of_day % 3_600 / 60
    )
}

/// A moment in UTC to the minute, `2026-10-23 06:12Z`: the browser's times are UTC, so a
/// screen reads the same on every machine.
pub(crate) fn utc_time(unix_ms: u64) -> String {
    let seconds = i64::try_from(unix_ms / 1_000).unwrap_or(i64::MAX / 2);
    let (year, month, day) = civil(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        of_day / 3_600,
        of_day % 3_600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_times_read_to_the_minute() {
        assert_eq!(utc_time(0), "1970-01-01 00:00Z");
        // 2026-10-23 06:12:59 UTC.
        assert_eq!(utc_time(1_792_735_979_000), "2026-10-23 06:12Z");
    }

    #[test]
    fn prose_wraps_at_spaces_and_keeps_a_quoted_command_whole() {
        let text = "provider architecture -> MISSING; fix: no codex Provider is registered: \
                    `af provider status` lists what this machine has, and `af provider setup \
                    codex-main --kind codex --login` registers one";
        for width in [100, 80] {
            let wrapped = wrap(text, width, 2);
            assert!(
                wrapped.lines().all(|line| line.chars().count() <= width),
                "{wrapped}"
            );
            assert!(wrapped.lines().skip(1).all(|line| line.starts_with("  ")));
            assert!(
                wrapped.contains("`af provider setup codex-main --kind codex --login`"),
                "{wrapped}"
            );
            assert_eq!(
                wrapped.split_whitespace().collect::<Vec<_>>().join(" "),
                text
            );
        }
        assert_eq!(wrap("short", 80, 2), "short");
        // A line that fits keeps its indent and its aligned columns.
        assert_eq!(wrap("  change  a  b", 80, 2), "  change  a  b");
        let long = format!("  empty  {}", "word ".repeat(30));
        let wrapped = wrap(long.trim_end(), 80, 9);
        assert!(wrapped.starts_with("  empty  word word"), "{wrapped}");
        assert!(
            wrapped
                .lines()
                .skip(1)
                .all(|line| line.starts_with("         word"))
        );
    }

    #[test]
    fn a_long_command_continues_between_its_arguments() {
        let line = "next:    af review run --policy-rev HEAD --base HEAD --uncommitted \
                    --provider correctness=claude-main --provider architecture=codex-main";
        assert_eq!(fit_command(line, 200), line);
        for width in [100, 80] {
            let fitted = fit_command(line, width);
            let lines: Vec<&str> = fitted.lines().collect();
            assert!(lines.len() > 1, "{fitted}");
            assert!(
                lines.iter().all(|row| row.chars().count() <= width),
                "{fitted}"
            );
            assert!(
                lines[..lines.len() - 1]
                    .iter()
                    .all(|row| row.ends_with(" \\"))
            );
            assert!(
                lines[1..]
                    .iter()
                    .all(|row| row.starts_with("             "))
            );
            // A shell reads the continued command as the one it was.
            let joined = fitted.replace(" \\\n", " ");
            let words = shell_words::split(joined.trim_start_matches("next:")).unwrap();
            assert_eq!(words, shell_words::split(&line[9..]).unwrap());
        }
        assert_eq!(fit_command(&"x".repeat(120), 80), "x".repeat(120));
    }

    /// #331 item 11: a Store selector wider than any line, a long `--state` path with a space
    /// and a quote in it, no longer leaves an over-wide `next:` line. It is cut into quoted
    /// pieces joined by a backslash and a newline; a shell reads the lines as the one command.
    #[test]
    fn a_selector_wider_than_a_line_is_cut_into_pieces_a_shell_joins() {
        let state = format!(
            "/private/var/folders/xy/T/.tmpAbC/{}/it's a state",
            "a-long-directory-name/".repeat(4)
        );
        let words = [
            "af",
            "task",
            "run",
            "pagination-cli",
            "--state",
            &state,
            "--repo",
            "repo",
        ];
        let line = format!("next: {}", shell_words::join(words));
        for width in [100, 80, 60, 40] {
            let fitted = fit_command(&line, width);
            assert!(
                fitted.lines().all(|row| row.chars().count() <= width),
                "{width}\n{fitted}"
            );
            // The selector's flag stays on a line that fits, and its value is never left whole
            // on a line of its own wider than the terminal.
            assert!(fitted.lines().count() > 2, "{fitted}");
            let joined = fitted.replace("\\\n", "");
            let read = shell_words::split(joined.trim_start_matches("next:")).unwrap();
            assert_eq!(read, words, "{width}\n{fitted}");
            // A POSIX shell, given the printed lines, receives exactly those words.
            let script = format!("printf '%s\\n' {}", fitted.trim_start_matches("next: af "));
            let printed = std::process::Command::new("/bin/sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert!(printed.status.success(), "{script}");
            assert_eq!(
                String::from_utf8(printed.stdout).unwrap(),
                words[1..]
                    .iter()
                    .map(|word| format!("{word}\n"))
                    .collect::<String>(),
                "{width}\n{fitted}"
            );
        }
        // Without a terminal on stdout a line is left whole for the script that reads it.
        use std::io::IsTerminal as _;
        if !std::io::stdout().is_terminal() {
            assert_eq!(fit_on_terminal(&line, false), line);
        }
    }

    /// The words a POSIX shell receives from `command`, the printed lines as pasted.
    fn sh_words(command: &str) -> Vec<String> {
        let printed = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("printf '%s\\n' {command}")])
            .output()
            .unwrap();
        assert!(printed.status.success(), "{command}");
        String::from_utf8(printed.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// #331 D6 and item 11: no line `fit_at` returns is wider than the terminal. A program path
    /// wider than a line (an `af` run by its absolute path) is cut like any argument; a label
    /// that leaves the command no room (the run log's `Task ID: STATE; next:` with a long Task
    /// ID) is wrapped first; a continuation is never indented past half the width. Each still
    /// reads, pasted into a shell, as the words it was.
    #[test]
    fn no_fitted_line_is_wider_than_the_terminal_label_and_program_included() {
        let program = format!(
            "/private/var/folders/f9/{}debug/af",
            "a-long-dir/".repeat(12)
        );
        let install = [program.as_str(), "self", "install", "0.14.0"];
        let id = "t".repeat(128);
        let state = format!("/tmp/{}state", "long-state-selector-".repeat(6));
        let deliver = [
            "af",
            "task",
            "deliver",
            id.as_str(),
            "--branch",
            "af/pagination-cli",
            "--state",
            state.as_str(),
        ];
        let cases = [
            (
                format!("                 fix: {}", shell_words::join(install)),
                "                 fix: ".len(),
                install.to_vec(),
            ),
            (
                format!(
                    "Task {id}: finished/gate_failed/check_failed; next: {}",
                    shell_words::join(deliver)
                ),
                format!("Task {id}: finished/gate_failed/check_failed; next: ").len(),
                deliver.to_vec(),
            ),
            (
                format!(
                    "Task pagination-cli: finished/gate_failed/check_failed; next: {}",
                    shell_words::join(deliver)
                ),
                "Task pagination-cli: finished/gate_failed/check_failed; next: ".len(),
                deliver.to_vec(),
            ),
        ];
        for (line, start, words) in cases {
            for width in [40, 60, 80, 100] {
                let fitted = fit_at(&line, start, width);
                for row in fitted.lines() {
                    assert!(row.chars().count() <= width, "{width}: {row:?}\n{fitted}");
                }
                for row in fitted.lines().skip(1) {
                    let indent = row.len() - row.trim_start().len();
                    assert!(indent <= width / 2, "{width}: {row:?}\n{fitted}");
                }
                // The command, from where it starts, is the words it was.
                let marker = if words[0] == "af" {
                    "af task "
                } else {
                    "/private/"
                };
                let at = fitted
                    .find(marker)
                    .unwrap_or_else(|| panic!("{width}\n{fitted}"));
                let command = &fitted[at..];
                assert_eq!(sh_words(command), words, "{width}\n{fitted}");
            }
        }
    }

    #[test]
    fn durations_read_in_their_two_largest_units() {
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(4_250), "4.2 s");
        assert_eq!(duration(185_000), "3 min 05 s");
        assert_eq!(duration(10_740_000), "2 h 59 min");
        assert_eq!(duration(3 * 86_400_000 + 4 * 3_600_000), "3 d 4 h");
    }

    #[test]
    fn civil_dates_and_utc_times() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_749), (2026, 10, 23));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    /// A version 1 TZif body with one transition: UTC before 1000 s, `XST` (+01:00) after.
    fn tzif() -> Vec<u8> {
        let mut data = b"TZif".to_vec();
        data.push(0);
        data.extend([0u8; 15]);
        for count in [0u32, 0, 0, 1, 2, 8] {
            data.extend(count.to_be_bytes());
        }
        data.extend(1000i32.to_be_bytes());
        data.push(1);
        data.extend(0i32.to_be_bytes());
        data.extend([0, 0]);
        data.extend(3600i32.to_be_bytes());
        data.extend([0, 4]);
        data.extend(b"UTC\0XST\0");
        data
    }

    #[test]
    fn a_zone_file_gives_the_offset_and_name_in_force() {
        let data = tzif();
        assert_eq!(tzif_zone(&data, 999), Some((0, "UTC".into())));
        assert_eq!(tzif_zone(&data, 1000), Some((3600, "XST".into())));
        assert_eq!(tzif_zone(b"not a zone", 0), None);
    }
}
