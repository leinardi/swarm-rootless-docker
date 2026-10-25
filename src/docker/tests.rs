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

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;
use tokio::time::Instant;

use super::*;

/// A scratch directory of this test process, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("srd-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An engine that sends the response headers at once and then stalls in the body: the call
/// fails with [`DockerError::Timeout`] after the per-call timeout, not whenever the body ends.
/// bollard's own timeout stops at the headers.
#[tokio::test]
async fn a_stalled_body_times_out() {
    let scratch = Scratch::new("stalled-body");
    let socket = scratch.0.join("docker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let engine = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n[",
            )
            .await
            .unwrap();
        // Hold the connection open, the body unfinished.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(stream);
    });

    let timeout = Duration::from_millis(300);
    let docker = Bollard::with_timeout(socket.to_str().unwrap(), timeout).unwrap();
    let started = Instant::now();
    let error = docker.list_containers("a=b", true).await.unwrap_err();
    let elapsed = started.elapsed();
    engine.abort();
    assert_eq!(error, DockerError::Timeout);
    assert!(elapsed >= timeout, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}
