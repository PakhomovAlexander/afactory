//! Phase spans for the performance laboratory's diagnostic runs (ADR-0174).
//!
//! The `perf-trace` feature is off by default, and every ordinary product build leaves it off:
//! then [`span`] returns a zero-sized guard with no destructor and [`finish`] does nothing, so a
//! call site costs nothing. A diagnostic build turns the feature on, and each [`span`] records
//! one closed [`Phase`] of the process (startup composition, canonicalization, CAS reads and
//! writes, projection, authority checks) into one bounded [`Recorder`]:
//!
//! - **Metadata only.** A span names a [`Phase`] from a closed list and nothing else: no payload,
//!   path, prompt, credential, argument or other caller text can reach the export.
//! - **Bounded.** At most [`CAPACITY`] spans are kept. A span that starts once they are taken is
//!   dropped and counted, and so are the spans nested in it, so a kept span's parent is always
//!   kept. A guard is not `Send`, so a span opens and closes on one thread, and its parent is the
//!   innermost span open on that thread.
//! - **Exported once.** [`finish`] writes `af.perf-trace/1` to the new file named by
//!   [`OUTPUT_ENV`], an absolute path the diagnostic driver chose; it never replaces a file.
//!   Timestamps are monotonic nanoseconds since the recorder's epoch, beside that epoch's wall
//!   time. A thread is the recorder's own process-local number, not an OS thread ID.
//!
//! Overlapping spans are never summed here or by the laboratory; the driver checks nesting per
//! thread and exports anything that does not nest as an explicit async track.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The export's schema.
pub const SCHEMA: &str = "af.perf-trace/1";

/// Whether this build records spans: the `perf-trace` feature.
pub const ENABLED: bool = cfg!(feature = "perf-trace");

/// The most spans one process keeps; later spans are counted as dropped.
pub const CAPACITY: usize = 65_536;

/// The environment variable naming the absolute path [`finish`] writes the export to.
pub const OUTPUT_ENV: &str = "AF_PERF_TRACE_OUTPUT";

/// The closed list of phases a span can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    /// `af` from `main` until its command is dispatched: argument parsing and setup.
    CliStartup,
    /// `af` running its dispatched command.
    CliCommand,
    /// RFC 8785 canonical serialization of one JSON value.
    Canonicalize,
    /// Validating a decoded envelope's identities.
    EnvelopeValidate,
    /// Storing one CAS object.
    CasPut,
    /// Reading and verifying one CAS object.
    CasGet,
    /// Reading, decoding and validating one typed CAS envelope.
    CasGetArtifact,
    /// Projecting one Task's state from its event log.
    TaskProjection,
    /// Checking the current plan before a Task records an effect.
    AuthorityPlanCheck,
    /// Constructing the Task runtime, which loads and checks the Task's authority.
    TaskRuntimeNew,
    /// The native probe's untimed fixture setup.
    ProbeSetup,
    /// The native probe's timed region.
    ProbeRegion,
}

impl Phase {
    pub const ALL: [Phase; 12] = [
        Phase::CliStartup,
        Phase::CliCommand,
        Phase::Canonicalize,
        Phase::EnvelopeValidate,
        Phase::CasPut,
        Phase::CasGet,
        Phase::CasGetArtifact,
        Phase::TaskProjection,
        Phase::AuthorityPlanCheck,
        Phase::TaskRuntimeNew,
        Phase::ProbeSetup,
        Phase::ProbeRegion,
    ];

    /// The phase's name in the export.
    pub fn name(self) -> &'static str {
        match self {
            Phase::CliStartup => "cli_startup",
            Phase::CliCommand => "cli_command",
            Phase::Canonicalize => "canonicalize",
            Phase::EnvelopeValidate => "envelope_validate",
            Phase::CasPut => "cas_put",
            Phase::CasGet => "cas_get",
            Phase::CasGetArtifact => "cas_get_artifact",
            Phase::TaskProjection => "task_projection",
            Phase::AuthorityPlanCheck => "authority_plan_check",
            Phase::TaskRuntimeNew => "task_runtime_new",
            Phase::ProbeSetup => "probe_setup",
            Phase::ProbeRegion => "probe_region",
        }
    }
}

/// One closed span: identity, nesting, phase, thread and its monotonic interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: u64,
    pub parent: Option<u64>,
    pub phase: Phase,
    pub thread: u64,
    pub start_ns: u64,
    pub end_ns: u64,
}

/// A span this thread opened and has not closed.
#[derive(Debug)]
struct Open {
    id: u64,
    parent: Option<u64>,
    phase: Phase,
    thread: u64,
    start_ns: u64,
}

thread_local! {
    /// This thread's number in each recorder that has seen it, by recorder serial.
    static THREADS: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
    /// The spans open on this thread, innermost last, with their recorder's serial; a dropped
    /// span is `None`, so the spans nested in it are dropped too.
    static OPEN: RefCell<Vec<(u64, Option<u64>)>> = const { RefCell::new(Vec::new()) };
}

/// The bounded recorder behind [`span`]. The global one exists only in a `perf-trace` build;
/// tests build their own.
#[derive(Debug)]
pub struct Recorder {
    serial: u64,
    capacity: usize,
    epoch: Instant,
    epoch_unix_ns: u128,
    reserved: AtomicUsize,
    dropped: AtomicU64,
    next_id: AtomicU64,
    next_thread: AtomicU64,
    records: Mutex<Vec<Record>>,
}

static SERIALS: AtomicU64 = AtomicU64::new(1);

impl Recorder {
    /// A recorder that keeps at most `capacity` spans, its epoch now.
    pub fn new(capacity: usize) -> Self {
        let epoch_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        Self {
            serial: SERIALS.fetch_add(1, Ordering::Relaxed),
            capacity,
            epoch: Instant::now(),
            epoch_unix_ns,
            reserved: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            next_thread: AtomicU64::new(1),
            records: Mutex::new(Vec::new()),
        }
    }

    fn now_ns(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn thread(&self) -> u64 {
        THREADS.with(|threads| {
            let mut threads = threads.borrow_mut();
            if let Some((_, number)) = threads.iter().find(|(serial, _)| *serial == self.serial) {
                return *number;
            }
            let number = self.next_thread.fetch_add(1, Ordering::Relaxed);
            threads.push((self.serial, number));
            number
        })
    }

    /// Open a span of `phase` on this thread.
    pub fn span(&self, phase: Phase) -> RecorderSpan<'_> {
        let parent = OPEN.with(|open| {
            open.borrow()
                .iter()
                .rev()
                .find(|(serial, _)| *serial == self.serial)
                .map(|(_, id)| *id)
        });
        // Inside a dropped span, or with no room left, this span is dropped too.
        let kept = !matches!(parent, Some(None))
            && self.reserved.fetch_add(1, Ordering::Relaxed) < self.capacity;
        let open = if kept {
            Some(Open {
                id: self.next_id.fetch_add(1, Ordering::Relaxed),
                parent: parent.flatten(),
                phase,
                thread: self.thread(),
                start_ns: self.now_ns(),
            })
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            None
        };
        OPEN.with(|stack| {
            stack
                .borrow_mut()
                .push((self.serial, open.as_ref().map(|open| open.id)))
        });
        RecorderSpan {
            recorder: self,
            open,
            _not_send: PhantomData,
        }
    }

    fn close(&self, open: Option<Open>) {
        let end_ns = self.now_ns();
        let id = open.as_ref().map(|open| open.id);
        OPEN.with(|stack| {
            let mut stack = stack.borrow_mut();
            // Innermost first: a guard dropped out of order still removes only itself.
            if let Some(index) = stack
                .iter()
                .rposition(|(serial, open)| *serial == self.serial && *open == id)
            {
                stack.remove(index);
            }
        });
        let Some(open) = open else { return };
        let record = Record {
            id: open.id,
            parent: open.parent,
            phase: open.phase,
            thread: open.thread,
            start_ns: open.start_ns,
            end_ns: end_ns.max(open.start_ns),
        };
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    /// The spans closed so far, ordered by start and then identity.
    pub fn records(&self) -> Vec<Record> {
        let mut records = self
            .records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default();
        records.sort_by_key(|record| (record.start_ns, record.id));
        records
    }

    /// The spans that found no room, or were nested in one that did not.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The `af.perf-trace/1` document of everything closed so far. Kept spans still open are
    /// counted, not exported.
    pub fn export(&self) -> Vec<u8> {
        let records = self.records();
        let kept = self.reserved.load(Ordering::Relaxed).min(self.capacity);
        let open = kept.saturating_sub(records.len());
        let spans: Vec<serde_json::Value> = records
            .iter()
            .map(|record| {
                serde_json::json!({
                    "end_ns": record.end_ns,
                    "id": record.id,
                    "parent": record.parent,
                    "phase": record.phase.name(),
                    "start_ns": record.start_ns,
                    "tid": record.thread,
                })
            })
            .collect();
        let document = serde_json::json!({
            "capacity": self.capacity,
            "clock": "monotonic_ns",
            "dropped": self.dropped(),
            "epoch_unix_ns": self.epoch_unix_ns.to_string(),
            "open": open,
            "pid": std::process::id(),
            "schema": SCHEMA,
            "spans": spans,
        });
        let mut bytes = serde_json::to_vec(&document).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }
}

/// A span of a [`Recorder`]; it closes when dropped, on the thread that opened it.
#[must_use = "a span ends when it is dropped"]
#[derive(Debug)]
pub struct RecorderSpan<'a> {
    recorder: &'a Recorder,
    open: Option<Open>,
    _not_send: PhantomData<*const ()>,
}

impl Drop for RecorderSpan<'_> {
    fn drop(&mut self) {
        self.recorder.close(self.open.take());
    }
}

/// A span of the process's recorder: zero-sized and inert unless the build has `perf-trace`.
#[must_use = "a span ends when it is dropped"]
#[derive(Debug)]
pub struct Span {
    #[cfg(feature = "perf-trace")]
    _inner: RecorderSpan<'static>,
    _not_send: PhantomData<*const ()>,
}

#[cfg(feature = "perf-trace")]
fn global() -> &'static Recorder {
    static GLOBAL: std::sync::OnceLock<Recorder> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(|| Recorder::new(CAPACITY))
}

/// Open a span of `phase` in this process, closed when the guard drops.
#[inline]
pub fn span(phase: Phase) -> Span {
    #[cfg(feature = "perf-trace")]
    {
        Span {
            _inner: global().span(phase),
            _not_send: PhantomData,
        }
    }
    #[cfg(not(feature = "perf-trace"))]
    {
        let _ = phase;
        Span {
            _not_send: PhantomData,
        }
    }
}

/// Export this process's spans to the new file [`OUTPUT_ENV`] names, at most once. Nothing
/// happens without `perf-trace` or without the variable; a failure is reported on stderr, so the
/// driver records the trace as missing rather than reading a partial one.
#[inline]
pub fn finish() {
    #[cfg(feature = "perf-trace")]
    {
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if DONE.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(path) = std::env::var_os(OUTPUT_ENV) {
            if let Err(error) = write_export(global(), std::path::Path::new(&path)) {
                eprintln!("perf-trace: {error}");
            }
        }
    }
}

/// Write `recorder`'s export to `path`, which must be absolute and must not exist yet: an
/// existing file is never replaced.
pub fn write_export(recorder: &Recorder, path: &std::path::Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!(
            "{OUTPUT_ENV} {} is not an absolute path; no trace written",
            path.display()
        ));
    }
    let bytes = recorder.export();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, &bytes))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn exported(recorder: &Recorder) -> Value {
        serde_json::from_slice(&recorder.export()).unwrap()
    }

    #[cfg(not(feature = "perf-trace"))]
    #[test]
    fn an_ordinary_build_records_nothing_and_its_span_is_zero_sized() {
        // The workspace's default features leave `perf-trace` off (ADR-0174): no product build
        // carries the recorder's work, and a call site's guard has no size and no destructor.
        assert!(!ENABLED, "a default build enabled perf-trace");
        assert_eq!(std::mem::size_of::<Span>(), 0);
        assert!(!std::mem::needs_drop::<Span>());
        let _span = span(Phase::CliStartup);
        finish();
    }

    #[cfg(feature = "perf-trace")]
    #[test]
    fn a_trace_build_records_the_process_spans_through_the_global_recorder() {
        assert!(ENABLED);
        {
            let _outer = span(Phase::CasGetArtifact);
            let _inner = span(Phase::CasGet);
        }
        let records = global().records();
        let outer = records
            .iter()
            .rev()
            .find(|record| record.phase == Phase::CasGetArtifact)
            .unwrap();
        assert!(
            records
                .iter()
                .any(|record| record.phase == Phase::CasGet && record.parent == Some(outer.id))
        );
    }

    #[test]
    fn an_export_is_written_once_to_a_new_absolute_file_and_never_replaces_one() {
        let recorder = Recorder::new(4);
        drop(recorder.span(Phase::CasPut));
        let refused = write_export(&recorder, std::path::Path::new("relative/trace.json"));
        assert!(refused.unwrap_err().contains("not an absolute path"));
        let dir = std::env::temp_dir().join(format!(
            "af-perf-trace-{}-{}",
            std::process::id(),
            recorder.serial
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.json");
        write_export(&recorder, &path).unwrap();
        let first = std::fs::read(&path).unwrap();
        let document: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(document["spans"].as_array().unwrap().len(), 1);
        let again = write_export(&recorder, &path).unwrap_err();
        assert!(again.contains("cannot write"), "{again}");
        assert_eq!(std::fs::read(&path).unwrap(), first);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spans_on_one_thread_nest_under_the_innermost_open_span() {
        let recorder = Recorder::new(16);
        {
            let _outer = recorder.span(Phase::CasGetArtifact);
            {
                let _first = recorder.span(Phase::CasGet);
            }
            let _second = recorder.span(Phase::EnvelopeValidate);
            let _inner = recorder.span(Phase::Canonicalize);
        }
        let records = recorder.records();
        assert_eq!(records.len(), 4);
        let by_phase = |phase| records.iter().find(|record| record.phase == phase).unwrap();
        let outer = by_phase(Phase::CasGetArtifact);
        assert_eq!(outer.parent, None);
        assert_eq!(by_phase(Phase::CasGet).parent, Some(outer.id));
        let second = by_phase(Phase::EnvelopeValidate);
        assert_eq!(second.parent, Some(outer.id));
        assert_eq!(by_phase(Phase::Canonicalize).parent, Some(second.id));
        for record in &records {
            assert!(record.start_ns <= record.end_ns);
            assert_eq!(record.thread, outer.thread);
            if let Some(parent) = record.parent {
                let parent = records.iter().find(|item| item.id == parent).unwrap();
                assert!(parent.start_ns <= record.start_ns && record.end_ns <= parent.end_ns);
            }
        }
        assert_eq!(recorder.dropped(), 0);
    }

    #[test]
    fn each_thread_is_its_own_track_and_never_parents_another_threads_span() {
        let recorder = Recorder::new(16);
        let _main = recorder.span(Phase::CliCommand);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _worker = recorder.span(Phase::TaskProjection);
            });
        });
        drop(_main);
        let records = recorder.records();
        let main = records
            .iter()
            .find(|record| record.phase == Phase::CliCommand)
            .unwrap();
        let worker = records
            .iter()
            .find(|record| record.phase == Phase::TaskProjection)
            .unwrap();
        assert_ne!(main.thread, worker.thread);
        assert_eq!(worker.parent, None);
    }

    #[test]
    fn spans_beyond_the_capacity_are_counted_and_their_children_dropped_with_them() {
        let recorder = Recorder::new(2);
        {
            let _a = recorder.span(Phase::CliStartup);
            let _b = recorder.span(Phase::Canonicalize);
            // No room: this span and everything nested in it are dropped and counted.
            let _c = recorder.span(Phase::CasGet);
            let _d = recorder.span(Phase::CasPut);
        }
        let _e = recorder.span(Phase::CasPut);
        drop(_e);
        let records = recorder.records();
        assert_eq!(records.len(), 2);
        assert_eq!(recorder.dropped(), 3);
        let ids: Vec<u64> = records.iter().map(|record| record.id).collect();
        for record in &records {
            assert!(record.parent.is_none_or(|parent| ids.contains(&parent)));
        }
        let document = exported(&recorder);
        assert_eq!(document["dropped"], 3);
        assert_eq!(document["capacity"], 2);
        assert_eq!(document["spans"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_span_dropped_out_of_order_still_closes_itself_only() {
        let recorder = Recorder::new(8);
        let outer = recorder.span(Phase::CliCommand);
        let inner = recorder.span(Phase::CasGet);
        drop(outer);
        let after = recorder.span(Phase::CasPut);
        drop(after);
        drop(inner);
        let records = recorder.records();
        let inner = records
            .iter()
            .find(|record| record.phase == Phase::CasGet)
            .unwrap();
        let after = records
            .iter()
            .find(|record| record.phase == Phase::CasPut)
            .unwrap();
        // The span opened after the outer one closed nests in the inner one, still open.
        assert_eq!(after.parent, Some(inner.id));
    }

    #[test]
    fn the_export_holds_only_identities_phases_threads_and_times() {
        // Payload exclusion is structural: a span takes only a closed phase, so the export's
        // strings are the schema, the clock, the epoch and phase names; nothing a caller handles
        // (a payload, a path, a prompt, a credential) has a way in.
        let recorder = Recorder::new(8);
        {
            let secret = "sk-ant-credential-and-a-prompt";
            let _span = recorder.span(Phase::CasPut);
            let _ = secret.len();
        }
        let open = recorder.span(Phase::ProbeRegion);
        let bytes = recorder.export();
        drop(open);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("sk-ant"));
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        let keys: Vec<&str> = document
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "capacity",
                "clock",
                "dropped",
                "epoch_unix_ns",
                "open",
                "pid",
                "schema",
                "spans"
            ]
        );
        assert_eq!(document["schema"], SCHEMA);
        assert_eq!(document["open"], 1, "a kept span still open is counted");
        assert_eq!(document["pid"], std::process::id());
        let names: Vec<&str> = Phase::ALL.iter().map(|phase| phase.name()).collect();
        for span in document["spans"].as_array().unwrap() {
            let keys: Vec<&str> = span
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, ["end_ns", "id", "parent", "phase", "start_ns", "tid"]);
            assert!(names.contains(&span["phase"].as_str().unwrap()));
        }
    }
}
