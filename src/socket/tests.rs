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

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

use super::*;

/// A directory of this test, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("srd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn socket(&self) -> PathBuf {
        self.0.join("test.sock")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const DEADLINE: Duration = Duration::from_millis(300);
/// The deadline plus what a loaded test machine may add before the timer fires.
const LATEST: Duration = Duration::from_millis(900);

#[test]
fn reads_the_whole_answer() {
    let scratch = Scratch::new("exchange-ok");
    let listener = UnixListener::bind(scratch.socket()).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 16];
        let len = stream.read(&mut request).unwrap();
        stream.write_all(b"pong").unwrap();
        request[..len].to_vec()
    });
    let answer = exchange(&scratch.socket(), b"ping", 1024, DEADLINE).unwrap();
    assert_eq!(answer, b"pong");
    assert_eq!(server.join().unwrap(), b"ping");
}

/// A listener that never accepts, its backlog (of one) full: the exchange fails within the
/// deadline instead of blocking in connect.
#[test]
fn a_full_backlog_fails_within_the_deadline() {
    let scratch = Scratch::new("exchange-backlog");
    // The listener and the connections that fill its backlog live on their own runtime, which
    // never accepts.
    let filler = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    let (listener, held) = filler.block_on(async {
        let socket = tokio::net::UnixSocket::new_stream().unwrap();
        socket.bind(scratch.socket()).unwrap();
        let listener = socket.listen(1).unwrap();
        let mut held = Vec::new();
        for _ in 0..64 {
            match tokio::net::UnixStream::connect(scratch.socket()).await {
                Ok(stream) => held.push(stream),
                Err(_) => break,
            }
        }
        (listener, held)
    });
    assert!(held.len() < 64, "the backlog never filled");

    let started = Instant::now();
    let error = exchange(&scratch.socket(), b"ping", 1024, DEADLINE).unwrap_err();
    assert!(started.elapsed() < LATEST, "{:?}", started.elapsed());
    assert!(
        matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ),
        "{error}"
    );
    drop((listener, held));
}

/// An answer trickled one byte at a time, each well within the deadline, fails at the deadline:
/// it bounds the whole read, not each byte.
#[test]
fn a_trickled_answer_fails_at_the_deadline() {
    let scratch = Scratch::new("exchange-trickle");
    let listener = UnixListener::bind(scratch.socket()).unwrap();
    let _server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 16];
        let _ = stream.read(&mut request).unwrap();
        for byte in b"HTTP/1.1 200 OK\r\n\r\nOK" {
            if stream.write_all(&[*byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    let started = Instant::now();
    let error = exchange(&scratch.socket(), b"ping", 1024, DEADLINE).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert!(started.elapsed() >= DEADLINE);
    assert!(started.elapsed() < LATEST, "{:?}", started.elapsed());
}
