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

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use super::*;

/// A directory of this test, removed on drop; the socket goes in a subdirectory `bind` creates.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("srd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("status").join("health.sock")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn only_a_running_healthy_daemon_is_healthy() {
    let running = |health: Option<&str>| Status::Running {
        health: health.map(str::to_owned),
    };
    assert_eq!(running(Some("healthy")).line(), "healthy");
    for status in [
        Status::Starting,
        Status::Stopping,
        running(Some("starting")),
        running(Some("unhealthy")),
        running(None),
    ] {
        assert!(status.line().starts_with("unhealthy: "), "{status:?}");
    }
}

/// The client reads what the launcher publishes now, as it changes; a launcher that is gone is
/// unhealthy. A socket left by an earlier run is replaced, and the directory is private.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_reads_the_current_status() {
    let scratch = Scratch::new("status");
    let socket = scratch.socket();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::fs::write(&socket, "stale").unwrap();
    let listener = bind(&socket).unwrap();
    let mode = std::fs::metadata(socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    let (publish, status) = watch::channel(Status::Starting);
    let server = tokio::spawn(serve(listener, status));
    let ask = |socket: PathBuf| {
        tokio::task::spawn_blocking(move || query(&socket, Duration::from_secs(2)))
    };
    let answer = ask(socket.clone()).await.unwrap().unwrap_err();
    assert_eq!(answer, "unhealthy: the daemon is not started yet");

    publish.send_replace(Status::Running {
        health: Some("healthy".to_owned()),
    });
    ask(socket.clone()).await.unwrap().unwrap();

    publish.send_replace(Status::Stopping);
    assert_eq!(
        ask(socket.clone()).await.unwrap().unwrap_err(),
        "unhealthy: stopping"
    );

    server.abort();
    let _ = server.await;
    std::fs::remove_file(&socket).unwrap();
    assert!(ask(socket).await.unwrap().unwrap_err().starts_with("ask "));
}
