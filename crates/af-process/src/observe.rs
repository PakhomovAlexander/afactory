//! A bounded observer of a supervised process's output, offered every chunk as the drain reads
//! it (design §7.3 of `docs/design/telemetry.md`).
//!
//! The drain threads stay the supervisor's: they read each pipe into the complete stream the
//! caller receives, exactly as without an observer. With one, each read also makes one
//! **non-blocking offer** of `(process, stream, byte_offset, bytes, arrival)` to a queue bounded
//! by bytes: a queue that has no room for the chunk drops it at once and counts it, and the
//! drain goes on reading. The offer takes no lock: the room is reserved with one atomic
//! operation, the chunk travels through a channel whose send never blocks, and the consumer is
//! woken with [`std::thread::Thread::unpark`], which never blocks and is never lost. So an
//! observer can neither delay the drain nor change what the supervisor collects, its status,
//! its deadline or anything it reports; a consumer that stalls, fails or goes away only makes
//! the queue fill and its chunks drop.
//!
//! This crate links no telemetry (ADR-0026): [`ObservedOutput`] is the consumer's end, which
//! the crate that receives the result drains into whatever records it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;

/// Which pipe a chunk was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObservedStream {
    Stdout,
    Stderr,
}

impl ObservedStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

/// One chunk as the drain read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedChunk {
    /// The supervised leader's process id.
    pub process: u32,
    pub stream: ObservedStream,
    /// The chunk's position in its stream: the bytes read from that pipe before it.
    pub byte_offset: u64,
    pub bytes: Vec<u8>,
    /// The drain's monotonic clock right after the read returned.
    pub arrival: Instant,
}

impl ObservedChunk {
    /// The arrival in nanoseconds since `origin` on the monotonic clock; 0 before it.
    pub fn arrival_mono_ns(&self, origin: Instant) -> u128 {
        self.arrival.saturating_duration_since(origin).as_nanos()
    }
}

struct Shared {
    capacity: u64,
    /// Bytes offered and not yet taken.
    queued: AtomicU64,
    /// Chunks dropped: no room, or offered after [`OutputObserver::close`].
    dropped: AtomicU64,
    /// Runs of consecutive dropped chunks, per stream: each is one gap the consumer sees.
    gaps: AtomicU64,
    /// Whether the last chunk offered on each stream was dropped.
    last_dropped: [AtomicBool; 2],
    /// Set once the supervisor returned: no chunk is offered after it.
    closed: AtomicBool,
    wake: Option<std::thread::Thread>,
}

/// The drain's end of the queue. Cloning shares it; [`observer`] makes one.
#[derive(Clone)]
pub struct OutputObserver {
    shared: Arc<Shared>,
    sender: mpsc::Sender<ObservedChunk>,
}

/// The consumer's end of the queue. Only the consumer takes from it, so its lock is never
/// contended by a drain.
pub struct ObservedOutput {
    shared: Arc<Shared>,
    receiver: std::sync::Mutex<mpsc::Receiver<ObservedChunk>>,
}

/// A queue of at most `capacity_bytes` of chunks, and the thread to wake when one arrives (the
/// consumer's; `None` when nobody waits and the queue is drained some other way).
pub fn observer(
    capacity_bytes: u64,
    wake: Option<std::thread::Thread>,
) -> (OutputObserver, ObservedOutput) {
    let shared = Arc::new(Shared {
        capacity: capacity_bytes,
        queued: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        gaps: AtomicU64::new(0),
        last_dropped: [AtomicBool::new(false), AtomicBool::new(false)],
        closed: AtomicBool::new(false),
        wake,
    });
    let (sender, receiver) = mpsc::channel();
    (
        OutputObserver {
            shared: Arc::clone(&shared),
            sender,
        },
        ObservedOutput {
            shared,
            receiver: std::sync::Mutex::new(receiver),
        },
    )
}

impl OutputObserver {
    /// Offer one chunk: never blocks. Without room for it, or after [`Self::close`], it is
    /// dropped at once and counted; whether it was queued.
    pub(crate) fn offer(
        &self,
        process: u32,
        stream: ObservedStream,
        byte_offset: u64,
        bytes: &[u8],
        arrival: Instant,
    ) -> bool {
        let shared = &*self.shared;
        let size = bytes.len() as u64;
        let reserved = !shared.closed.load(Ordering::Acquire)
            && shared
                .queued
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                    queued
                        .checked_add(size)
                        .filter(|total| *total <= shared.capacity)
                })
                .is_ok();
        let sent = reserved
            && self
                .sender
                .send(ObservedChunk {
                    process,
                    stream,
                    byte_offset,
                    bytes: bytes.to_vec(),
                    arrival,
                })
                .is_ok();
        if reserved && !sent {
            shared.queued.fetch_sub(size, Ordering::AcqRel);
        }
        let was_dropped = shared.last_dropped[stream.index()].swap(!sent, Ordering::AcqRel);
        if !sent {
            shared.dropped.fetch_add(1, Ordering::AcqRel);
            if !was_dropped {
                shared.gaps.fetch_add(1, Ordering::AcqRel);
            }
            return false;
        }
        if let Some(thread) = &shared.wake {
            thread.unpark();
        }
        true
    }

    /// The supervisor returned: no chunk is offered after this, and the consumer, once it has
    /// taken every queued chunk, has the whole observed stream.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Some(thread) = &self.shared.wake {
            thread.unpark();
        }
    }

    /// Chunks dropped so far.
    pub fn dropped_chunks(&self) -> u64 {
        self.shared.dropped.load(Ordering::Acquire)
    }

    /// Gaps so far: runs of consecutive dropped chunks of one stream.
    pub fn gaps(&self) -> u64 {
        self.shared.gaps.load(Ordering::Acquire)
    }
}

impl ObservedOutput {
    /// The oldest queued chunk, without waiting.
    pub fn try_take(&self) -> Option<ObservedChunk> {
        let chunk = self
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_recv()
            .ok()?;
        self.shared
            .queued
            .fetch_sub(chunk.bytes.len() as u64, Ordering::AcqRel);
        Some(chunk)
    }

    /// Whether a chunk is queued.
    pub fn has_queued(&self) -> bool {
        self.shared.queued.load(Ordering::Acquire) > 0
    }

    /// Whether the supervisor returned ([`OutputObserver::close`]): once the queue is empty
    /// after this, nothing more arrives.
    pub fn closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// Chunks dropped so far.
    pub fn dropped_chunks(&self) -> u64 {
        self.shared.dropped.load(Ordering::Acquire)
    }

    /// Gaps so far.
    pub fn gaps(&self) -> u64 {
        self.shared.gaps.load(Ordering::Acquire)
    }
}

/// Where the drain of one pipe offers its chunks.
#[derive(Clone)]
pub(crate) struct Tap {
    pub observer: OutputObserver,
    pub process: u32,
    pub stream: ObservedStream,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_drops_at_once_and_counts_runs_as_gaps() {
        let (observer, output) = super::observer(8, None);
        let now = Instant::now();
        assert!(observer.offer(1, ObservedStream::Stdout, 0, b"abcd", now));
        assert!(observer.offer(1, ObservedStream::Stdout, 4, b"efgh", now));
        // No room: dropped, one gap for the run of two.
        assert!(!observer.offer(1, ObservedStream::Stdout, 8, b"ijkl", now));
        assert!(!observer.offer(1, ObservedStream::Stdout, 12, b"mnop", now));
        assert_eq!((observer.dropped_chunks(), observer.gaps()), (2, 1));
        let first = output.try_take().unwrap();
        assert_eq!(
            (first.byte_offset, first.bytes.as_slice()),
            (0, &b"abcd"[..])
        );
        assert!(observer.offer(1, ObservedStream::Stdout, 16, b"qrst", now));
        // A second run is a second gap; the other stream keeps its own runs.
        assert!(!observer.offer(1, ObservedStream::Stdout, 20, b"uvwx", now));
        assert!(!observer.offer(1, ObservedStream::Stderr, 0, b"12345", now));
        assert_eq!((observer.dropped_chunks(), observer.gaps()), (4, 3));
        let offsets: Vec<u64> = std::iter::from_fn(|| output.try_take())
            .map(|chunk| chunk.byte_offset)
            .collect();
        assert_eq!(offsets, [4, 16]);
        assert!(!output.has_queued());
    }

    #[test]
    fn nothing_is_queued_after_close_or_without_a_consumer() {
        let (observer, output) = super::observer(1 << 20, None);
        observer.close();
        assert!(output.closed());
        assert!(!observer.offer(7, ObservedStream::Stderr, 0, b"late", Instant::now()));
        assert_eq!(output.dropped_chunks(), 1);
        let (observer, output) = super::observer(1 << 20, None);
        drop(output);
        // A consumer that went away: the chunk is dropped, never an error of the drain.
        assert!(!observer.offer(7, ObservedStream::Stdout, 0, b"gone", Instant::now()));
        assert_eq!((observer.dropped_chunks(), observer.gaps()), (1, 1));
    }

    #[test]
    fn an_offer_wakes_the_consumer_thread() {
        let (ready, started) = mpsc::channel();
        let consumer = std::thread::spawn(move || {
            ready.send(()).unwrap();
            // Woken by the offer: an unpark before the park is never lost.
            std::thread::park();
        });
        started.recv().unwrap();
        let (observer, output) = super::observer(64, Some(consumer.thread().clone()));
        assert!(observer.offer(1, ObservedStream::Stdout, 0, b"x", Instant::now()));
        consumer.join().unwrap();
        assert_eq!(output.try_take().unwrap().bytes, b"x");
    }
}
