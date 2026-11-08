//! The supervisor owns deadlines; only this disposable writer can block on a pipe.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    io::Write,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
    time::Instant,
};

const CAPACITY: usize = 8;

struct Frame {
    id: u64,
    bytes: Vec<u8>,
}

pub(super) struct Transport {
    sender: Option<SyncSender<Frame>>,
    completed: Receiver<(u64, Instant, std::io::Result<()>)>,
    writer: Option<JoinHandle<()>>,
    pending: Option<Frame>,
    deadlines: BTreeMap<u64, Instant>,
    next_id: u64,
}

impl Transport {
    pub(super) fn start(mut pipe: impl Write + Send + 'static) -> Result<Self> {
        let (sender, frames) = mpsc::sync_channel::<Frame>(CAPACITY);
        let (completion, completed) = mpsc::channel();
        let writer = thread::Builder::new()
            .name("routing-stdin".into())
            .spawn(move || {
                for frame in frames {
                    let result = pipe.write_all(&frame.bytes).and_then(|()| pipe.flush());
                    let failed = result.is_err();
                    if completion.send((frame.id, Instant::now(), result)).is_err() || failed {
                        break;
                    }
                }
            })
            .context("start routing stdin writer")?;
        Ok(Self {
            sender: Some(sender),
            completed,
            writer: Some(writer),
            pending: None,
            deadlines: BTreeMap::new(),
            next_id: 0,
        })
    }

    pub(super) fn ready(&self) -> bool {
        self.pending.is_none()
    }

    #[cfg(test)]
    pub(super) fn has_unfinished_write(&self) -> bool {
        !self.deadlines.is_empty() && self.completed.try_iter().count() == 0
    }

    /// Reserve the deadline before offering bytes. Full is ordinary backpressure:
    /// retain one unsent frame and let the supervisor continue its watchdog loop.
    pub(super) fn enqueue(&mut self, json: String, deadline: Instant) -> Result<()> {
        ensure!(self.ready(), "routing stdin already has a pending frame");
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("routing stdin sequence exhausted")?;
        let mut bytes = json.into_bytes();
        bytes.push(b'\n');
        self.deadlines.insert(id, deadline);
        self.pending = Some(Frame { id, bytes });
        Ok(())
    }

    /// Stop supersedes only the not-yet-enqueued frame. Previously offered
    /// frames retain FIFO order; Stop intent itself is outside this bounded queue.
    pub(super) fn stop(&mut self, json: String, deadline: Instant) -> Result<()> {
        if let Some(frame) = self.pending.take() {
            self.deadlines.remove(&frame.id);
        }
        self.enqueue(json, deadline)
    }

    pub(super) fn poll(&mut self, now: Instant) -> Result<()> {
        while let Ok((id, finished, result)) = self.completed.try_recv() {
            result.context("write routing worker stdin")?;
            if let Some(due) = self.deadlines.remove(&id) {
                ensure!(
                    finished < due,
                    "routing worker stdin write exceeded its deadline"
                );
            }
        }
        // Writer timestamps distinguish a late write from a timely completion
        // observed after a supervisor scheduling delay. Neither extends a deadline.
        ensure!(
            self.deadlines.values().all(|due| now < *due),
            "routing worker stdin timed out; worker terminated. Restart target applications before retrying"
        );
        if let Some(frame) = self.pending.take() {
            match self
                .sender
                .as_ref()
                .context("routing stdin closed")?
                .try_send(frame)
            {
                Ok(()) => {}
                Err(TrySendError::Full(frame)) => self.pending = Some(frame),
                Err(TrySendError::Disconnected(_)) => bail!("routing stdin writer exited"),
            }
        }
        Ok(())
    }

    pub(super) fn close(&mut self) {
        self.sender.take();
        self.pending.take();
    }

    /// Only call after the owned worker/job has exited, closing its pipe reader.
    pub(super) fn join_after_worker_exit(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct Blocked {
        entered: mpsc::Sender<()>,
        release: Receiver<()>,
    }
    impl Write for Blocked {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            let _ = self.entered.send(());
            let _ = self.release.recv();
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_writer_keeps_watchdog_stop_and_backpressure_nonblocking() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut transport = Transport::start(Blocked {
            entered,
            release: gate,
        })
        .unwrap();
        let now = Instant::now();
        let due = now + Duration::from_secs(10);
        transport.enqueue("profile".into(), due).unwrap();
        transport.poll(now).unwrap();
        let blocked = started.recv_timeout(Duration::from_secs(2)).is_ok();
        // One in-flight write, eight queued frames and one pending frame.
        let mut accepted = true;
        for _ in 0..=CAPACITY {
            accepted &= transport.enqueue("request".into(), due).is_ok();
            accepted &= transport.poll(now).is_ok();
        }
        let backpressured = !transport.ready();
        let stop = transport.stop("Stop".into(), due + Duration::from_secs(20));
        let stop_poll = transport.poll(now);
        let expired = transport.poll(due).is_err();
        transport.close();
        // Simulates exact worker exit breaking the blocked pipe; never join first.
        let _ = release.send(());
        transport.join_after_worker_exit();
        assert!(blocked && accepted && backpressured);
        assert!(stop.is_ok() && stop_poll.is_ok() && expired);
    }

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_preserves_frames_and_write_completion_is_only_transport_progress() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut transport = Transport::start(Capture(bytes.clone())).unwrap();
        let now = Instant::now();
        for frame in ["profile", "press", "release", "Stop"] {
            transport
                .enqueue(frame.into(), now + Duration::from_secs(10))
                .unwrap();
            transport.poll(now).unwrap();
        }
        transport.close();
        transport.join_after_worker_exit();
        assert_eq!(*bytes.lock().unwrap(), b"profile\npress\nrelease\nStop\n");
        // No Event/commit callback exists in Transport: only worker ACKs can
        // satisfy the supervisor's operation/cleanup contract.
        transport.poll(now).unwrap();
        assert!(transport.deadlines.is_empty());
    }
}
