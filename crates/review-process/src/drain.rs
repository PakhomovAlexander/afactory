use crate::{OUTPUT_DRAIN_GRACE, SupervisedError, kill_process_group};
use std::io::Read;
use std::sync::{Arc, Mutex, mpsc};

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
    let _ = drain.completion.recv_timeout(OUTPUT_DRAIN_GRACE);
    drain.take_bytes()
}

pub(crate) fn collect(drain: Drain, stream: &'static str, pid: u32) -> Drained {
    let status = match drain.completion.recv_timeout(OUTPUT_DRAIN_GRACE) {
        Ok(Ok(())) => Ok(false),
        Ok(Err(source)) => Err(SupervisedError::OutputRead { stream, source }),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            kill_process_group(pid);
            if stream == "stderr" {
                Ok(true)
            } else {
                Err(SupervisedError::OutputHeld(stream))
            }
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
    let drained = collect(drain, "stderr", pid);
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
        let drained = collect(drain_async(BrokenReader(false)), "stdout", u32::MAX);
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
}
