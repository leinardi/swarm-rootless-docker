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
    // The text names this client's timeout, not the default CALL_TIMEOUT.
    assert_eq!(
        error,
        DockerError::Timeout("no complete answer within 300ms".to_owned())
    );
    assert!(elapsed >= timeout, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

/// A socket nobody listens on any more: the call fails with moby's text for a refused dial,
/// naming the socket, where bollard's own text named no host.
#[tokio::test]
async fn a_refused_dial_is_worded_as_moby_words_it() {
    let scratch = Scratch::new("refused-dial");
    let socket = scratch.0.join("docker.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let path = socket.to_str().unwrap();
    let docker = Bollard::with_timeout(path, Duration::from_secs(5)).unwrap();
    let error = docker.list_containers("a=b", true).await.unwrap_err();
    assert_eq!(
        error,
        DockerError::Other(format!(
            "Cannot connect to the Docker daemon at unix://{path}. Is the docker daemon running?"
        ))
    );
}

/// A socket that does not exist is refused when the client is built, with moby's text for that
/// dial; the launcher puts its `connect to <socket>` action before it.
#[test]
fn a_missing_socket_is_worded_as_moby_words_it() {
    let scratch = Scratch::new("missing-socket");
    let socket = scratch.0.join("docker.sock");
    let path = socket.to_str().unwrap();
    let error = Bollard::with_timeout(path, Duration::from_secs(5)).unwrap_err();
    assert_eq!(
        error,
        DockerError::Other(format!(
            "failed to connect to the docker API at unix://{path}; check if the path is correct \
             and if the daemon is running: dial unix {path}: connect: no such file or directory"
        ))
    );
}

/// An engine that takes the attach over and sends multiplexed frames: the request is moby's
/// hijacked `ContainerAttach` (stdout, stderr, stream, `Upgrade: tcp`, moby's `Host`), and the
/// frames come back as stdout and stderr in the order sent, a frame header split across writes
/// included; the stream ends when the engine closes the connection.
#[tokio::test]
async fn an_attach_reads_the_multiplexed_frames_in_order() {
    let scratch = Scratch::new("attach");
    let socket = scratch.0.join("docker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let engine = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "the request ended before its headers");
            request.extend_from_slice(&chunk[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 101 UPGRADED\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
            )
            .await
            .unwrap();
        stream
            .write_all(b"\x01\x00\x00\x00\x00\x00\x00\x03out")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        stream.write_all(b"\x02\x00\x00\x00").await.unwrap();
        stream.flush().await.unwrap();
        stream.write_all(b"\x00\x00\x00\x04err\n").await.unwrap();
        stream
            .write_all(b"\x01\x00\x00\x00\x00\x00\x00\x04\x01log")
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        String::from_utf8(request).unwrap().to_ascii_lowercase()
    });

    let docker = Bollard::with_timeout(socket.to_str().unwrap(), Duration::from_secs(5)).unwrap();
    let mut outputs = docker.attach("abc").await.unwrap();
    let mut received = Vec::new();
    while let Some(output) = outputs.recv().await {
        received.push(output.unwrap());
    }
    let request = engine.await.unwrap();
    assert!(
        request.starts_with("post /")
            && request.contains("/containers/abc/attach?stderr=1&stdout=1&stream=1 http/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\nhost: api.moby.localhost\r\n"),
        "{request}"
    );
    assert!(request.contains("\r\nupgrade: tcp\r\n"), "{request}");
    assert!(request.contains("\r\nconnection: upgrade\r\n"), "{request}");
    // The engine's frames, in order; a payload starting with `\x01` is not mistaken for a header.
    assert_eq!(
        received,
        vec![
            Output::Stdout(b"out".to_vec()),
            Output::Stderr(b"err\n".to_vec()),
            Output::Stdout(b"\x01log".to_vec()),
        ]
    );
}
