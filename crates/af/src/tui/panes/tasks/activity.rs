//! The inspector's independent incremental reader. The key loop only receives bounded snapshots;
//! dropping a selection stops its reader without waiting for filesystem IO.
use af_store::activity::{Binding, Error, Reader, Record};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::time::Duration;

#[derive(Default)]
pub(super) struct Snapshot {
    pub records: Vec<Record>,
    pub sequence: u64,
    pub lost: u64,
    pub error: Option<Error>,
}

pub(super) struct Job {
    pub binding: Binding,
    pub snapshot: Snapshot,
    receiver: Receiver<Snapshot>,
    stop: Arc<AtomicBool>,
    thread: std::thread::Thread,
}
impl Job {
    pub fn start(binding: Binding) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let reading = binding.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("af-activity-read".into())
            .spawn(move || {
                let mut reader = Reader::new(reading);
                while !stopping.load(Ordering::Acquire) {
                    let error = reader.poll().err();
                    let snapshot = Snapshot {
                        records: reader.records.iter().cloned().collect(),
                        sequence: reader.sequence,
                        lost: reader.lost,
                        error,
                    };
                    if matches!(
                        send.try_send(snapshot),
                        Err(mpsc::TrySendError::Disconnected(_))
                    ) {
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(250));
                }
            })
            .ok()?;
        Some(Self {
            binding,
            snapshot: Snapshot::default(),
            receiver,
            stop,
            thread: thread.thread().clone(),
        })
    }
    pub fn poll(&mut self) -> bool {
        if let Ok(snapshot) = self.receiver.try_recv() {
            self.snapshot = snapshot;
            true
        } else {
            false
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

#[cfg(test)]
impl Job {
    pub(super) fn receive(&mut self) {
        self.snapshot = self.receiver.recv().expect("activity reader remains alive");
    }

    pub(super) fn fed(binding: Binding) -> (Self, mpsc::SyncSender<Snapshot>) {
        let (send, receiver) = mpsc::sync_channel(1);
        (
            Self {
                binding,
                snapshot: Snapshot::default(),
                receiver,
                stop: Arc::new(AtomicBool::new(false)),
                thread: std::thread::current(),
            },
            send,
        )
    }
}
