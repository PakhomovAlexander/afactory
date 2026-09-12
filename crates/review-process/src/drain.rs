use crate::{OUTPUT_DRAIN_GRACE, SupervisedError, kill_process_group};
use std::io::Read;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub(crate) struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    completion: mpsc::Receiver<std::io::Result<()>>,
}

impl Drain {
    fn take_bytes(&self) -> Vec<u8> {
        std::mem::take(&mut *self.bytes.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

pub(crate) struct Drained {
    pub bytes: Vec<u8>,
    pub status: Result<bool, SupervisedError>,
}

pub(crate) fn drain_async(mut pipe: impl Read + Send + 'static) -> Drain {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let accumulated = Arc::clone(&bytes);
    let (send, completion) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0; 16 * 1024];
        let result = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(count) => accumulated
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = send.send(result);
    });
    Drain { bytes, completion }
}

/// Failure cleanup retains every prefix, even if EOF or a successful read never arrives.
pub(crate) fn collect_after_kill(drain: Drain) -> Vec<u8> {
    collect_after_kill_until(drain, Instant::now() + OUTPUT_DRAIN_GRACE)
}

pub(crate) fn collect_after_kill_until(drain: Drain, deadline: Instant) -> Vec<u8> {
    let _ = drain
        .completion
        .recv_timeout(deadline.saturating_duration_since(Instant::now()));
    drain.take_bytes()
}

pub(crate) fn collect(
    drain: Drain,
    stream: &'static str,
    pid: u32,
    cleanup: &mut Option<Instant>,
) -> Drained {
    collect_with(
        drain,
        stream,
        pid,
        cleanup,
        OUTPUT_DRAIN_GRACE,
        kill_process_group,
    )
}

fn collect_with(
    drain: Drain,
    stream: &'static str,
    pid: u32,
    cleanup: &mut Option<Instant>,
    grace: Duration,
    kill: impl FnOnce(u32),
) -> Drained {
    let deadline = cleanup.unwrap_or_else(|| Instant::now() + grace);
    let status = match drain
        .completion
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        Ok(Ok(())) => Ok(false),
        Ok(Err(source)) => Err(SupervisedError::OutputRead { stream, source }),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let deadline = *cleanup.get_or_insert_with(|| Instant::now() + grace);
            kill(pid);
            // The reader may have obtained a final chunk without appending it yet. Wait for
            // bounded completion before taking its bytes. Both pipes share this deadline.
            let bytes = collect_after_kill_until(drain, deadline);
            let status = if stream == "stderr" {
                Ok(true)
            } else {
                Err(SupervisedError::OutputHeld(stream))
            };
            return Drained { bytes, status };
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SupervisedError::OutputRead {
            stream,
            source: std::io::Error::other("output reader stopped without a result"),
        }),
    };
    Drained {
        bytes: drain.take_bytes(),
        status,
    }
}

pub(crate) fn collect_stderr(drain: Drain, pid: u32) -> Result<(Vec<u8>, bool), SupervisedError> {
    let drained = collect(drain, "stderr", pid, &mut None);
    drained.status.map(|held| (drained.bytes, held))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenReader(bool);
    impl Read for BrokenReader {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("fixture read failure"));
            }
            self.0 = true;
            target[..6].copy_from_slice(b"prefix");
            Ok(6)
        }
    }

    #[test]
    fn a_read_failure_retains_the_prefix() {
        let drained = collect_with(
            drain_async(BrokenReader(false)),
            "stdout",
            0,
            &mut None,
            OUTPUT_DRAIN_GRACE,
            |_| {},
        );
        assert_eq!(drained.bytes, b"prefix");
        assert!(matches!(
            drained.status,
            Err(SupervisedError::OutputRead {
                stream: "stdout",
                ..
            })
        ));
    }

    #[test]
    fn cleanup_retains_a_prefix_even_when_its_reader_failed() {
        assert_eq!(
            collect_after_kill(drain_async(BrokenReader(false))),
            b"prefix"
        );
    }

    #[test]
    fn held_pipe_cleanup_keeps_a_chunk_delivered_after_group_termination() {
        struct DelayedReader {
            begun: Option<mpsc::Sender<()>>,
            killed: mpsc::Receiver<()>,
        }
        impl Read for DelayedReader {
            fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
                let Some(begun) = self.begun.take() else {
                    return Err(std::io::Error::other("read failed after the final prefix"));
                };
                target[..6].copy_from_slice(b"prefix");
                begun.send(()).unwrap();
                self.killed.recv().unwrap();
                std::thread::sleep(Duration::from_millis(30));
                Ok(6)
            }
        }
        for stream in ["stdout", "stderr"] {
            let (begun, ready) = mpsc::channel();
            let (kill, killed) = mpsc::channel();
            let drain = drain_async(DelayedReader {
                begun: Some(begun),
                killed,
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            let mut cleanup = None;
            let drained = collect_with(
                drain,
                stream,
                0,
                &mut cleanup,
                Duration::from_secs(1),
                |_| kill.send(()).unwrap(),
            );
            assert_eq!(drained.bytes, b"prefix");
            if stream == "stdout" {
                assert!(matches!(
                    drained.status,
                    Err(SupervisedError::OutputHeld("stdout"))
                ));
            } else {
                assert!(matches!(drained.status, Ok(true)));
            }
            let deadline = cleanup.unwrap();
            // Subsequent cleanup cannot start another grace period after this deadline.
            let _ = collect_with(
                drain_async(std::io::Cursor::new(b"second")),
                "stderr",
                0,
                &mut cleanup,
                Duration::from_secs(1),
                |_| {},
            );
            assert_eq!(cleanup, Some(deadline));
        }
    }
}
