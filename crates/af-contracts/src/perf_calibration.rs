//! Deterministic CPU-loop injection for the performance laboratory's calibration cohorts
//! (ADR-0181).
//!
//! A slowdown trial measures whether the laboratory detects work it knows was added. Both sides
//! of a trial run the same calibration-enabled build of one source: only the candidate's
//! environment names a nonzero [`ITERATIONS_ENV`]. The loop runs inside the timed operation (the
//! `perf-probe` region, or the `af` process itself), after the operation's own work, and touches
//! no state, so every oracle sees the result it would see without it. It is pure arithmetic: no
//! sleep, no clock wait, no wrapper delay and no reported metric that the loop did not take. Its
//! measured duration is reported as one stderr line, [`REPORT_SCHEMA`], beside the process's
//! unchanged output.
//!
//! Without the default-off `perf-calibration` feature, which no product or budget build enables,
//! nothing here runs a loop, and a process whose environment names [`ITERATIONS_ENV`] at all is
//! refused by [`configure_from_env`] rather than measured as if the control did not exist.

/// The environment variable naming the loop's iteration count, a canonical decimal integer.
pub const ITERATIONS_ENV: &str = "AF_PERF_CALIBRATION_ITERATIONS";
/// The schema word that starts the one stderr line reporting the measured injection.
pub const REPORT_SCHEMA: &str = "af.perf-calibration-injection/1";
/// The most iterations one process may be told to run.
pub const ITERATIONS_MAX: u64 = 1 << 40;

#[cfg(feature = "perf-calibration")]
static CONFIGURED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
#[cfg(feature = "perf-calibration")]
static INJECTED: std::sync::Mutex<Option<(u64, u128)>> = std::sync::Mutex::new(None);

/// Whether this build carries the calibration loop.
pub const fn enabled() -> bool {
    cfg!(feature = "perf-calibration")
}

/// The iteration count a value of [`ITERATIONS_ENV`] names: a canonical decimal integer within
/// `0..=ITERATIONS_MAX`, no sign, no leading zero and no surrounding text.
pub fn parse_iterations(value: &str) -> Result<u64, String> {
    let canonical = !value.is_empty()
        && value.len() <= 14
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'));
    let iterations = canonical
        .then(|| value.parse::<u64>().ok())
        .flatten()
        .filter(|iterations| *iterations <= ITERATIONS_MAX);
    iterations.ok_or_else(|| {
        format!(
            "{ITERATIONS_ENV}={value:?} is not a canonical iteration count within 0..={ITERATIONS_MAX}"
        )
    })
}

/// Read the process's injection control once, before any timed work. A build without the
/// feature refuses any value at all; a calibration build refuses a malformed one and otherwise
/// arms the loop (an absent variable is no injection, as is `0`).
pub fn configure_from_env() -> Result<(), String> {
    let value = std::env::var_os(ITERATIONS_ENV);
    configure(
        value
            .as_ref()
            .map(|value| value.to_str().unwrap_or("\u{fffd}")),
    )
}

fn configure(value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !enabled() {
        return Err(format!(
            "{ITERATIONS_ENV} is set, but this build has no perf-calibration feature; a production \
             or budget build refuses every injection control"
        ));
    }
    let iterations = parse_iterations(value)?;
    #[cfg(feature = "perf-calibration")]
    CONFIGURED.store(iterations, std::sync::atomic::Ordering::SeqCst);
    let _ = iterations;
    Ok(())
}

/// The deterministic loop: `iterations` steps of a 64-bit linear congruential generator whose
/// state the optimizer must keep, returning that state. It reads and writes nothing else.
pub fn spin(iterations: u64) -> u64 {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for step in 0..iterations {
        state = std::hint::black_box(
            state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(step | 1),
        );
    }
    state
}

/// Run the armed loop once, inside the caller's timed operation, and keep its measured duration
/// for [`report_line`]. Inert, and `None`, without the feature or when nothing was configured;
/// a second call in one process runs nothing more.
pub fn inject() -> Option<u128> {
    #[cfg(feature = "perf-calibration")]
    {
        let iterations = CONFIGURED.swap(u64::MAX, std::sync::atomic::Ordering::SeqCst);
        if iterations == u64::MAX {
            return None;
        }
        let started = std::time::Instant::now();
        std::hint::black_box(spin(iterations));
        let nanos = started.elapsed().as_nanos();
        *INJECTED.lock().unwrap_or_else(|error| error.into_inner()) = Some((iterations, nanos));
        Some(nanos)
    }
    #[cfg(not(feature = "perf-calibration"))]
    {
        None
    }
}

/// The one stderr line of the injection this process ran, if it ran one.
pub fn report_line() -> Option<String> {
    #[cfg(feature = "perf-calibration")]
    {
        let injected = *INJECTED.lock().unwrap_or_else(|error| error.into_inner());
        injected.map(|(iterations, nanos)| format_report(iterations, nanos))
    }
    #[cfg(not(feature = "perf-calibration"))]
    {
        None
    }
}

/// `af.perf-calibration-injection/1 iterations=N injected_ns=M`.
pub fn format_report(iterations: u64, nanos: u128) -> String {
    format!("{REPORT_SCHEMA} iterations={iterations} injected_ns={nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_counts_are_canonical_and_bounded() {
        assert_eq!(parse_iterations("0"), Ok(0));
        assert_eq!(parse_iterations("1500000"), Ok(1_500_000));
        assert_eq!(
            parse_iterations(&ITERATIONS_MAX.to_string()),
            Ok(ITERATIONS_MAX)
        );
        for bad in [
            "",
            "01",
            "-1",
            "+1",
            "1e6",
            " 1",
            "1 ",
            "1099511627777",
            "x",
        ] {
            assert!(parse_iterations(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_loop_is_deterministic_arithmetic() {
        assert_eq!(spin(0), 0x9e37_79b9_7f4a_7c15);
        assert_eq!(spin(1000), spin(1000));
        assert_ne!(spin(1000), spin(1001));
        assert_eq!(
            format_report(7, 1234),
            "af.perf-calibration-injection/1 iterations=7 injected_ns=1234"
        );
    }

    #[cfg(not(feature = "perf-calibration"))]
    #[test]
    fn an_absent_control_injects_nothing() {
        assert_eq!(configure(None), Ok(()));
        assert_eq!(inject(), None);
    }

    #[cfg(not(feature = "perf-calibration"))]
    #[test]
    fn a_build_without_the_feature_refuses_every_control() {
        for value in ["0", "1000", "garbage"] {
            let refused = configure(Some(value)).unwrap_err();
            assert!(refused.contains("no perf-calibration feature"), "{refused}");
        }
        assert_eq!(inject(), None);
        assert_eq!(report_line(), None);
    }

    #[cfg(feature = "perf-calibration")]
    #[test]
    fn a_calibration_build_runs_the_declared_loop_once_and_reports_it() {
        assert!(configure(Some("01")).is_err());
        configure(Some("200000")).unwrap();
        let nanos = inject().expect("armed");
        assert!(nanos > 0);
        assert_eq!(inject(), None, "one injection per process");
        let line = report_line().unwrap();
        assert!(
            line.starts_with("af.perf-calibration-injection/1 iterations=200000 injected_ns="),
            "{line}"
        );
    }
}
