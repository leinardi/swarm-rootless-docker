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

/// The whole exec, pinned: the entrypoint, `dockerd` first (never the default-arguments path),
/// only the Unix socket, and an environment of exactly three variables.
#[test]
fn dockerd_exec_is_pinned() {
    assert_eq!(
        dockerd(568),
        Exec {
            program: "/usr/local/bin/dockerd-entrypoint.sh".to_owned(),
            args: vec![
                "dockerd".to_owned(),
                "--host=unix:///home/rootless/socket/docker.sock".to_owned()
            ],
            env: vec![
                (
                    "PATH".to_owned(),
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned()
                ),
                ("HOME".to_owned(), "/home/rootless".to_owned()),
                ("XDG_RUNTIME_DIR".to_owned(), "/run/user/568".to_owned()),
            ],
        }
    );
}

/// The launcher's archive passes; anything else refuses.
#[test]
fn subid_files() {
    assert_eq!(
        check_subids("/etc/subuid", "rootless:100000:65536\n").unwrap(),
        IdRange::parse("100000:65536").unwrap()
    );
    for bad in [
        "",
        "rootless:100000:65536\nrootless:300000:65536\n",
        "root:100000:65536\n",
        "rootless:1000:65536\n",
        "rootless:100000\n",
        "rootless:100000:65536:x\n",
    ] {
        assert!(
            matches!(
                check_subids("/etc/subuid", bad),
                Err(DaemonError::Subids { .. })
            ),
            "{bad:?} accepted"
        );
    }
}

/// A socket directory of this test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("srd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn socket(&self) -> String {
        self.0.join("docker.sock").to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A one-shot engine on `socket` that reads the request and answers `response`, or never answers
/// when it is `None`.
fn spawn_engine(socket: &str, response: Option<&'static str>) -> thread::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let len = stream.read(&mut request).unwrap();
        match response {
            Some(response) => stream.write_all(response.as_bytes()).unwrap(),
            None => thread::sleep(Duration::from_secs(2)),
        }
        String::from_utf8_lossy(&request[..len]).into_owned()
    })
}

#[test]
fn ping_accepts_only_ok() {
    let scratch = Scratch::new("ping-ok");
    let engine = spawn_engine(
        &scratch.socket(),
        Some("HTTP/1.1 200 OK\r\nApi-Version: 1.53\r\nContent-Length: 2\r\n\r\nOK"),
    );
    ping(&scratch.socket(), Duration::from_secs(1)).unwrap();
    assert!(
        engine
            .join()
            .unwrap()
            .starts_with("GET /_ping HTTP/1.0\r\n")
    );

    for (name, response) in [
        (
            "server error",
            "HTTP/1.1 500 Internal Server Error\r\n\r\nOK",
        ),
        ("other body", "HTTP/1.1 200 OK\r\n\r\nNOT OK"),
        ("not HTTP", "OK"),
    ] {
        let scratch = Scratch::new(&format!("ping-{}", name.replace(' ', "-")));
        let engine = spawn_engine(&scratch.socket(), Some(response));
        let error = ping(&scratch.socket(), Duration::from_secs(1)).unwrap_err();
        assert!(
            matches!(error, DaemonError::Unhealthy(_)),
            "{name}: {error}"
        );
        engine.join().unwrap();
    }
}

/// No socket, or a dockerd that never answers: unhealthy, within the timeout.
#[test]
fn ping_fails_fast() {
    let scratch = Scratch::new("ping-missing");
    let error = ping(&scratch.socket(), Duration::from_secs(1)).unwrap_err();
    assert!(matches!(error, DaemonError::Io { .. }), "{error}");

    let scratch = Scratch::new("ping-stall");
    let _engine = spawn_engine(&scratch.socket(), None);
    let started = Instant::now();
    let error = ping(&scratch.socket(), Duration::from_millis(200)).unwrap_err();
    assert!(matches!(error, DaemonError::Io { .. }), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
}
