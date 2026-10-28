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

//! The launcher's status: what it publishes on a Unix socket inside its own container, and the
//! `healthcheck` subcommand (the image's `HEALTHCHECK`) that reads it.
//!
//! The launcher is healthy only while its daemon runs and Docker reports the daemon's own
//! healthcheck as healthy; the answer is one line, `healthy` or `unhealthy: <why>`. No clock is
//! involved: the launcher updates the status when it inspects the daemon, and a launcher that
//! stopped answering is unhealthy by the client's timeout.

#[cfg(test)]
mod tests;

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixListener;
use tokio::sync::watch;

/// Where the launcher listens, inside its own container.
pub const STATUS_SOCKET: &str = "/tmp/swarm-rootless-docker/health.sock";
/// How long the `healthcheck` client may take, from the connect to the end of the answer.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the launcher spends writing one answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1);
const HEALTHY: &str = "healthy";
const MAX_ANSWER: u64 = 1024;
const DIR_MODE: u32 = 0o700;

/// What the launcher knows about its daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Before the daemon was started.
    Starting,
    /// The daemon runs; its Docker health status (`starting`, `healthy`, `unhealthy`), or `None`
    /// when Docker reported none.
    Running { health: Option<String> },
    /// Shutting down.
    Stopping,
}

impl Status {
    /// The one-line answer.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Running { health } if health.as_deref() == Some(HEALTHY) => HEALTHY.to_owned(),
            Self::Running { health } => format!(
                "unhealthy: the daemon's health is {}",
                health.as_deref().unwrap_or("unknown")
            ),
            Self::Starting => "unhealthy: the daemon is not started yet".to_owned(),
            Self::Stopping => "unhealthy: stopping".to_owned(),
        }
    }
}

/// Binds the status socket at `path`: its directory created private to this user, and a socket
/// left by an earlier run replaced.
///
/// # Errors
///
/// The I/O error of creating the directory or binding.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        match std::fs::create_dir(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    UnixListener::bind(path)
}

/// Answers every connection with the current status line, until the task is dropped. A client
/// that does not read is given up after `ANSWER_TIMEOUT` (one second).
pub async fn serve(listener: UnixListener, status: watch::Receiver<Status>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let line = format!("{}\n", status.borrow().line());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(ANSWER_TIMEOUT, stream.write_all(line.as_bytes())).await;
        });
    }
}

/// The `healthcheck` subcommand: asks the launcher at `path`, the connect and the whole answer
/// within `deadline`.
///
/// # Errors
///
/// The reason it is not healthy: the launcher's own answer, or why it gave none in time.
pub fn query(path: &Path, deadline: Duration) -> Result<(), String> {
    let answer = crate::socket::exchange(path, b"", MAX_ANSWER, deadline)
        .map_err(|error| format!("ask {}: {error}", path.display()))?;
    let answer = String::from_utf8_lossy(&answer);
    match answer.lines().next().unwrap_or_default() {
        HEALTHY => Ok(()),
        "" => Err("the launcher closed without answering".to_owned()),
        answer => Err(answer.to_owned()),
    }
}
