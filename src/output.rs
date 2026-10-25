/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! Writing to stdout and stderr off the runtime.
//!
//! The launcher runs on one runtime thread. A write to a pipe nobody reads blocks, and on that
//! thread it would stall everything: the stop signals, the status socket, the Docker calls and
//! the shutdown timers. So each stream is a [`Lane`]: a bounded queue drained by a thread of its
//! own. A full queue makes an async sender wait (only the task that sends) or a sync sender drop
//! the piece; nothing on the runtime ever waits for the stream itself. A writer thread that is
//! stuck does not keep the process alive.

use std::io::{self, Write};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, error::TrySendError};
use tracing_subscriber::fmt::MakeWriter;

/// Pieces queued per lane before senders wait or drop.
pub const LANE_CAPACITY: usize = 1024;
/// How often a flush retries a full queue.
const FLUSH_RETRY: Duration = Duration::from_millis(10);

enum Message {
    Bytes(Vec<u8>),
    /// Answered once every piece queued before it was written.
    Flush(std_mpsc::Sender<()>),
}

/// A stream written by its own thread.
#[derive(Debug, Clone)]
pub struct Lane(mpsc::Sender<Message>);

impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(formatter, "Bytes({})", bytes.len()),
            Self::Flush(_) => formatter.write_str("Flush"),
        }
    }
}

impl Lane {
    /// Starts the thread `name` writing to `writer`. Write errors are dropped: a closed stream
    /// must not stop the launcher.
    ///
    /// # Errors
    ///
    /// The thread cannot be started.
    pub fn spawn(name: &str, mut writer: impl Write + Send + 'static) -> io::Result<Self> {
        let (sender, mut receiver) = mpsc::channel(LANE_CAPACITY);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Some(message) = receiver.blocking_recv() {
                    match message {
                        Message::Bytes(bytes) => {
                            let _ = writer.write_all(&bytes).and_then(|()| writer.flush());
                        }
                        Message::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        Ok(Self(sender))
    }

    /// Queues `bytes`, waiting for room: only the sending task waits.
    pub async fn send(&self, bytes: Vec<u8>) {
        let _ = self.0.send(Message::Bytes(bytes)).await;
    }

    /// Queues `bytes` if there is room, dropping them otherwise. Whether they were queued.
    #[must_use]
    pub fn try_send(&self, bytes: Vec<u8>) -> bool {
        self.0.try_send(Message::Bytes(bytes)).is_ok()
    }

    /// Blocks until everything queued so far is written, or `timeout` passes. Whether it was.
    /// For the end of the process, off the runtime.
    #[must_use]
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (done, written) = std_mpsc::channel();
        let mut message = Message::Flush(done);
        loop {
            match self.0.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                    message = back;
                    std::thread::sleep(FLUSH_RETRY);
                }
                Err(TrySendError::Full(_) | TrySendError::Closed(_)) => return false,
            }
        }
        written
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }
}

/// A lane as a tracing writer: each event is queued, or dropped when the lane is full.
#[derive(Debug)]
pub struct LaneWriter(Lane);

impl Write for LaneWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Dropped when the lane is full: losing a log line beats blocking the runtime.
        let _ = self.0.try_send(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for Lane {
    type Writer = LaneWriter;

    fn make_writer(&self) -> Self::Writer {
        LaneWriter(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer that blocks until the test lets it go.
    struct Stuck(std_mpsc::Receiver<()>);

    impl Write for Stuck {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn pieces_arrive_in_order_once_flushed() {
        let captured = Captured::default();
        let lane = Lane::spawn("test-lane", captured.clone()).unwrap();
        lane.send(b"one ".to_vec()).await;
        assert!(lane.try_send(b"two".to_vec()));
        assert!(lane.flush(Duration::from_secs(5)));
        assert_eq!(*captured.0.lock().unwrap(), b"one two");
    }

    /// A stuck writer fills the lane: sync senders drop, and nothing on the caller's thread
    /// blocks; a flush gives up at its timeout.
    #[test]
    fn a_stuck_writer_never_blocks_the_sender() {
        let (release, stuck) = std_mpsc::channel();
        let lane = Lane::spawn("test-stuck", Stuck(stuck)).unwrap();
        let queued = (0..LANE_CAPACITY * 2)
            .filter(|_| lane.try_send(b"x".to_vec()))
            .count();
        assert!(queued <= LANE_CAPACITY + 1, "{queued}");
        let started = Instant::now();
        assert!(!lane.flush(Duration::from_millis(50)));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(release);
    }
}
