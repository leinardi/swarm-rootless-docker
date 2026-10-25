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

//! The daemon container's side: the `daemon` subcommand, which starts the rootless dockerd, and
//! `daemon-health`, its healthcheck.
//!
//! `daemon` takes no argument and execs the upstream entrypoint with fixed arguments and a fixed
//! environment. The fixed first argument `dockerd` keeps the entrypoint off its default-arguments
//! path, which would add a TCP listener; the cleared environment keeps out the variables the
//! entrypoint passes to rootlesskit (`DOCKERD_ROOTLESS_ROOTLESSKIT_FLAGS` and its siblings) and
//! `DOCKER_HOST`. Before it, the subcommand checks what the launcher promised: a non-root user and
//! root-owned `/etc/subuid` and `/etc/subgid` holding one `rootless` range each.

#[cfg(test)]
mod tests;

use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::config::IdRange;
use crate::spec::SOCKET_PATH;

/// The upstream image's entrypoint.
pub const DOCKERD_ENTRYPOINT: &str = "/usr/local/bin/dockerd-entrypoint.sh";
/// The `PATH` dockerd and rootlesskit run with: the upstream image's.
pub const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// The rootless user's home, where the data root and socket directory are mounted.
pub const HOME: &str = "/home/rootless";
/// How long `daemon-health` may take, from the connect to the end of dockerd's answer.
pub const PING_TIMEOUT: Duration = Duration::from_secs(3);

const SUBUID: &str = "/etc/subuid";
const SUBGID: &str = "/etc/subgid";
const RUNTIME_DIR_MODE: u32 = 0o700;
const MAX_RESPONSE: u64 = 4096;

/// Why the daemon cannot start, or is not healthy.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("the daemon must not run as root")]
    Root,
    #[error("{file}: {reason}")]
    Subids { file: &'static str, reason: String },
    #[error("{action}: {source}")]
    Io { action: String, source: io::Error },
    #[error("dockerd answered {0:?}")]
    Unhealthy(String),
}

/// A program to exec: its path, arguments and whole environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// What `daemon` execs for `uid`: the entrypoint, `dockerd` with only the Unix socket in the
/// socket directory, and only `PATH`, `HOME` and `XDG_RUNTIME_DIR` set.
#[must_use]
pub fn dockerd(uid: u32) -> Exec {
    Exec {
        program: DOCKERD_ENTRYPOINT.to_owned(),
        args: vec!["dockerd".to_owned(), format!("--host=unix://{SOCKET_PATH}")],
        env: vec![
            ("PATH".to_owned(), PATH.to_owned()),
            ("HOME".to_owned(), HOME.to_owned()),
            ("XDG_RUNTIME_DIR".to_owned(), runtime_dir(uid)),
        ],
    }
}

/// `XDG_RUNTIME_DIR` for `uid`, under the `/run/user` tmpfs.
#[must_use]
pub fn runtime_dir(uid: u32) -> String {
    format!("{}/{uid}", crate::spec::RUN_USER)
}

/// Checks one subordinate ID file's content: exactly one line, `rootless:START:COUNT`, with a
/// range the launcher would accept.
///
/// # Errors
///
/// [`DaemonError::Subids`] naming what is wrong.
pub fn check_subids(file: &'static str, text: &str) -> Result<IdRange, DaemonError> {
    let reason = |reason: String| DaemonError::Subids { file, reason };
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let [line] = lines.as_slice() else {
        return Err(reason(format!("{} lines, not one", lines.len())));
    };
    let range = line
        .strip_prefix("rootless:")
        .ok_or_else(|| reason(format!("{line:?} is not for rootless")))?;
    IdRange::parse(range).map_err(|error| reason(format!("{line:?}: {error}")))
}

/// Runs the `daemon` subcommand: checks, then execs dockerd. Returns only on failure.
#[must_use]
pub fn run() -> DaemonError {
    let uid = rustix::process::getuid().as_raw();
    if uid == 0 {
        return DaemonError::Root;
    }
    for file in [SUBUID, SUBGID] {
        if let Err(error) = check_subid_file(file) {
            return error;
        }
    }
    let dir = runtime_dir(uid);
    if let Err(error) = make_runtime_dir(&dir) {
        return io_error(&format!("create {dir}"), error);
    }
    let exec = dockerd(uid);
    let error = Command::new(&exec.program)
        .args(&exec.args)
        .env_clear()
        .envs(exec.env.iter().map(|(key, value)| (key, value)))
        .exec();
    io_error(&format!("exec {}", exec.program), error)
}

/// Runs `daemon-health`: `GET /_ping` on dockerd's socket.
///
/// # Errors
///
/// A [`DaemonError`] when dockerd does not answer `200 OK` within [`PING_TIMEOUT`], all told.
pub fn health() -> Result<(), DaemonError> {
    ping(SOCKET_PATH, PING_TIMEOUT)
}

/// `GET /_ping` on the Docker socket at `socket`: the connect, the request and the whole answer
/// within `deadline`.
///
/// # Errors
///
/// A [`DaemonError`] for a connection or I/O failure, the deadline passing, or any answer but
/// `200` with body `OK`.
pub fn ping(socket: &str, deadline: Duration) -> Result<(), DaemonError> {
    let answer = crate::socket::exchange(
        Path::new(socket),
        b"GET /_ping HTTP/1.0\r\nHost: docker\r\n\r\n",
        MAX_RESPONSE,
        deadline,
    )
    .map_err(|error| io_error(&format!("ping {socket}"), error))?;
    let response = String::from_utf8_lossy(&answer);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.lines().next().unwrap_or_default();
    let mut parts = status.split(' ');
    let healthy = matches!(
        (parts.next(), parts.next()),
        (Some("HTTP/1.0" | "HTTP/1.1"), Some("200"))
    ) && body == "OK";
    if healthy {
        Ok(())
    } else {
        Err(DaemonError::Unhealthy(status.to_owned()))
    }
}

/// The launcher uploaded the file root-owned and not writable by the daemon user; anything else
/// means it is not the launcher's.
fn check_subid_file(file: &'static str) -> Result<IdRange, DaemonError> {
    let metadata = std::fs::symlink_metadata(file)
        .map_err(|error| io_error(&format!("stat {file}"), error))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(DaemonError::Subids {
            file,
            reason: format!(
                "not a root-owned file writable only by root (uid {}, mode {:o})",
                metadata.uid(),
                metadata.mode() & 0o7777
            ),
        });
    }
    let text =
        std::fs::read_to_string(file).map_err(|error| io_error(&format!("read {file}"), error))?;
    check_subids(file, &text)
}

fn make_runtime_dir(dir: &str) -> io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(RUNTIME_DIR_MODE))
}

fn io_error(action: &str, source: io::Error) -> DaemonError {
    DaemonError::Io {
        action: action.to_owned(),
        source,
    }
}
