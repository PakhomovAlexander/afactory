//! The native performance probe of the continuous performance laboratory (ADR-0163).
//!
//! One process measures one sample: it reads one bounded `af.perf-probe-request/2` from stdin,
//! generates the case's fixture through the product's public writers in the declared scratch
//! (or opens the sample's private copy of the seed its side wrote once, ADR-0170), times the
//! case's production operation once per declared repetition, and writes one
//! `af.perf-probe/1` line to stdout. Its self-reported `passed` is not a verdict: the trusted
//! driver (`python3 scripts/performance/cli.py probe`) checks every message against the
//! authority's golden entries. The helper modules below are the probe's complete closure, so
//! an authority copy of these files can be compiled in any measured worktree whose public APIs
//! they call, with no dependency beyond the crate's own. A diagnostic build may add the crate's
//! default-off `perf-trace` spans and `perf-control` region handshake (ADR-0174); a verdict build
//! has neither. Only a calibration trial's build has `perf-calibration`, whose loop runs inside
//! the timed region and is reported on stderr (ADR-0181). A driven profiling build runs each further repetition as a fresh
//! execution of its own image (ADR-0175).

use std::io::{Read, Write};
use std::process::ExitCode;

#[path = "perf-probe/authority.rs"]
mod authority;
#[path = "perf-probe/cases.rs"]
mod cases;
#[path = "perf-probe/control.rs"]
mod control;
#[path = "perf-probe/fixtures.rs"]
mod fixtures;
#[path = "perf-probe/protocol.rs"]
mod protocol;

fn main() -> ExitCode {
    // A slowdown trial's calibration loop (ADR-0181): armed here, run inside the timed region;
    // a build without `perf-calibration` refuses its control before reading the request.
    if let Err(error) = review_core::perf_calibration::configure_from_env() {
        eprintln!("perf-probe: {error}");
        return ExitCode::from(2);
    }
    let mut input = Vec::new();
    let limit = protocol::RECORD_BYTES_MAX as u64 + 1;
    if let Err(error) = std::io::stdin().take(limit).read_to_end(&mut input) {
        eprintln!("perf-probe: cannot read the request: {error}");
        return ExitCode::from(2);
    }
    let reply = protocol::run(&input);
    // A driven profiling build delivers its message on the region channel and executes afresh
    // for each further repetition (ADR-0175); every other build writes it to stdout once.
    let driven = control::driven();
    if driven {
        control::deliver(&reply.stdout);
    }
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let written = (if driven {
        Ok(())
    } else {
        stdout
            .write_all(&reply.stdout)
            .and_then(|()| stdout.flush())
    })
    .and_then(|()| stderr.write_all(&reply.stderr))
    .and_then(|()| match review_core::perf_calibration::report_line() {
        Some(line) => writeln!(stderr, "{line}"),
        None => Ok(()),
    });
    drop((stdout, stderr));
    if written.is_err() {
        return ExitCode::from(1);
    }
    if reply.code == 0 {
        // Returns only when the driver wants no further repetition.
        control::another();
    }
    // Only a `perf-trace` diagnostic build records and exports phase spans.
    review_core::perf_trace::finish();
    ExitCode::from(reply.code)
}
