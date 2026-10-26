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

//! The `launch` mode: create this instance's daemon container through the host socket, run it,
//! and tie its life to the launcher's, so Swarm owns the daemon through the launcher. Port of
//! swarm-device-access `internal/launcher`.
//!
//! 1. Identify the launcher's own container, its image and the host paths no daemon may touch
//!    (the engine's data root and the host socket).
//! 2. Under the node lock: survey the node, resolve the network when there are aliases, probe the
//!    host paths, check them and the aliases against the other daemons, create the daemon; then release the lock. The locked section is never cancelled:
//!    a stop request is only looked at once the lock is released, and every step in it is
//!    bounded.
//! 3. Upload the daemon's `/etc/subuid` and `/etc/subgid`, attach its output to the launcher's
//!    own, start it, and only then register the wait (Docker's `not-running` condition answers at
//!    once for a created container).
//! 4. Supervise: the daemon's exit ends the launcher with the daemon's status; a lost log stream
//!    or a failed wait stops the daemon and fails; `SIGTERM` or `SIGINT` stops it; its health is
//!    published on the status socket.
//!
//! Every container the launcher makes is removed by ID on every path it controls; what a crash
//! or `SIGKILL` leaves, the next launcher on the node removes ([`crate::node`]).

mod probe;
#[cfg(test)]
mod tests;

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use crate::config::{AbsolutePath, LaunchConfig};
use crate::docker::{
    Bollard, CALL_TIMEOUT, ContainerInfo, DockerApi, DockerError, ExitWait, Output, OutputStream,
};
use crate::mounts::{self, MountError};
use crate::node::{self, NodeError, NodeLock, short};
use crate::output::Lane;
use crate::spec::{self, ContainerId, DAEMON_STOP_TIMEOUT_SECS, ImageIdentity, SpecError};
use crate::status::{self, Status};

/// Where the launcher reaches the host socket, inside its own container.
pub const DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// The launcher's own mount table, where its container ID shows.
pub const SELF_MOUNTINFO: &str = "/proc/self/mountinfo";
/// Where the daemon's subordinate ID files go.
const SUBID_DIR: &str = "/etc";
/// The files Docker bind-mounts from the container's own directory into every container.
const CONTAINER_FILE_TARGETS: [&str; 3] = ["/etc/hostname", "/etc/hosts", "/etc/resolv.conf"];
/// The exit code of a launcher error.
pub const EXIT_FAILURE: u8 = 1;

/// The launcher cannot go on. Every one ends it with [`EXIT_FAILURE`].
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("identify the launcher container: {0}")]
    SelfId(String),
    #[error("{action}: {source}")]
    Docker { action: String, source: DockerError },
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Node(#[from] NodeError),
    #[error("the host-side probe refused the configuration: {0}")]
    ProbeRefused(String),
    #[error("the host-side probe failed: {0}")]
    Probe(String),
    #[error("build the subordinate ID files: {0}")]
    Archive(io::Error),
    #[error("the daemon's log stream was lost while it ran: {0}")]
    LogStream(DockerError),
    #[error("the wait for the daemon's exit failed: {0}")]
    Wait(String),
    #[error("the daemon did not stop within {0:?}; it was removed by force")]
    StopTimeout(Duration),
    #[error("the daemon was not removed within {0:?}")]
    RemoveTimeout(Duration),
    #[error("set up the launcher: {0}")]
    Setup(io::Error),
}

/// How long each phase may take. The defaults are the production values; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The probe, from its start to its report.
    pub probe: Duration,
    /// Between two looks at the daemon's health.
    pub health: Duration,
    /// Shutdown, first part: the stop (`SIGTERM`, then `SIGKILL` after
    /// [`DAEMON_STOP_TIMEOUT_SECS`]) and the daemon's exit.
    pub stop: Duration,
    /// Shutdown, second part, and after the daemon exited by itself: the remove and the last of
    /// its output.
    pub finish: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            probe: Duration::from_secs(20),
            health: Duration::from_secs(10),
            // The stop timeout, plus the time to kill and report the exit.
            stop: Duration::from_secs(DAEMON_STOP_TIMEOUT_SECS.unsigned_abs() + 5),
            finish: CALL_TIMEOUT,
        }
    }
}

/// Where the daemon's output goes: the launcher's own stdout and stderr, so Swarm's service logs
/// carry dockerd's. Each is a [`Lane`]: a stream nobody reads holds back only the copy of the
/// daemon's output, never the launcher.
#[derive(Debug, Clone)]
pub struct Console {
    stdout: Lane,
    stderr: Lane,
}

impl Console {
    /// The process's stdout and stderr.
    ///
    /// # Errors
    ///
    /// A writer thread cannot be started.
    pub fn process() -> io::Result<Self> {
        Self::new(io::stdout(), io::stderr())
    }

    /// Any two writers.
    ///
    /// # Errors
    ///
    /// A writer thread cannot be started.
    pub fn new(
        stdout: impl Write + Send + 'static,
        stderr: impl Write + Send + 'static,
    ) -> io::Result<Self> {
        Ok(Self {
            stdout: Lane::spawn("daemon-stdout", stdout)?,
            stderr: Lane::spawn("daemon-stderr", stderr)?,
        })
    }

    /// Blocks until what was queued is written, or `timeout` passes; off the runtime.
    #[must_use]
    pub fn flush(&self, timeout: Duration) -> bool {
        let stdout = self.stdout.flush(timeout);
        self.stderr.flush(timeout) && stdout
    }

    async fn write(&self, output: Output) {
        match output {
            Output::Stdout(bytes) => self.stdout.send(bytes).await,
            Output::Stderr(bytes) => self.stderr.send(bytes).await,
        }
    }
}

/// One launch: what it needs from outside, injected so tests can drive it.
pub struct Launch<'run, Docker: DockerApi> {
    pub docker: &'run Docker,
    pub config: &'run LaunchConfig,
    /// The launcher's own container.
    pub launcher: ContainerId,
    pub console: Console,
    /// What the status socket answers.
    pub status: watch::Sender<Status>,
    /// Turns `true` when the launcher is asked to stop.
    pub shutdown: watch::Receiver<bool>,
    pub timing: Timing,
}

/// The `launch` subcommand in production: the host socket, the launcher's own container, the
/// status socket and the stop signals, with the daemon's output to `console`. Returns the exit
/// code.
pub async fn launch(config: &LaunchConfig, console: Console) -> u8 {
    match launch_with_process(config, console).await {
        Ok(code) => code,
        Err(error) => {
            tracing::error!(%error, "the launcher failed");
            EXIT_FAILURE
        }
    }
}

async fn launch_with_process(config: &LaunchConfig, console: Console) -> Result<u8, LaunchError> {
    // First, so a stop request that arrives during the setup is not lost.
    let shutdown = stop_requests().map_err(LaunchError::Setup)?;
    let table = std::fs::read_to_string(SELF_MOUNTINFO)
        .map_err(|error| LaunchError::SelfId(format!("read {SELF_MOUNTINFO}: {error}")))?;
    let launcher = self_id(&table)?;
    let docker = Bollard::connect(DOCKER_SOCKET).map_err(|source| LaunchError::Docker {
        action: format!("connect to {DOCKER_SOCKET}"),
        source,
    })?;
    let (status, published) = watch::channel(Status::Starting);
    let listener = status::bind(Path::new(status::STATUS_SOCKET)).map_err(LaunchError::Setup)?;
    let server = tokio::spawn(status::serve(listener, published));
    tracing::info!(
        version = crate::version::long(),
        launcher = short(launcher.as_str()),
        instance = %config.instance(),
        "launcher starting"
    );
    let result = Launch {
        docker: &docker,
        config,
        launcher,
        console,
        status,
        shutdown,
        timing: Timing::default(),
    }
    .run()
    .await;
    server.abort();
    result
}

/// A flag that turns `true` on the first `SIGTERM` or `SIGINT`.
fn stop_requests() -> io::Result<watch::Receiver<bool>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let (sender, receiver) = watch::channel(false);
    tokio::spawn(async move {
        let name = tokio::select! {
            _ = terminate.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        };
        tracing::info!(signal = name, "stop requested");
        let _ = sender.send(true);
        // Keep the sender, so the flag stays readable.
        std::future::pending::<()>().await;
    });
    Ok(receiver)
}

/// The launcher's own container ID, from its mount table: Docker bind-mounts each container's
/// `hostname`, `hosts` and `resolv.conf` from `…/containers/<ID>/` onto `/etc`. Port of
/// swarm-device-access `parseSelfID`.
///
/// # Errors
///
/// [`LaunchError::SelfId`] when the table does not parse, names no container, or names two.
pub fn self_id(table: &str) -> Result<ContainerId, LaunchError> {
    let mounts = mounts::parse(table).map_err(|error: MountError| {
        LaunchError::SelfId(format!("parse {SELF_MOUNTINFO}: {error}"))
    })?;
    let mut found: Option<ContainerId> = None;
    for mount in &mounts {
        if !CONTAINER_FILE_TARGETS.contains(&mount.mount_point.as_str()) {
            continue;
        }
        let Some(id) = container_file_owner(&mount.root) else {
            continue;
        };
        match &found {
            Some(earlier) if *earlier != id => {
                return Err(LaunchError::SelfId(
                    "more than one Docker container ID in the mount table".to_owned(),
                ));
            }
            Some(_) | None => found = Some(id),
        }
    }
    found.ok_or_else(|| {
        LaunchError::SelfId(
            "no Docker container ID in the mount table; is the launcher running in a container?"
                .to_owned(),
        )
    })
}

/// The container ID in a root ending in `/containers/<ID>/(hostname|hosts|resolv.conf)`.
fn container_file_owner(root: &str) -> Option<ContainerId> {
    let mut parts = root.rsplitn(4, '/');
    let file = parts.next()?;
    let id = parts.next()?;
    let containers = parts.next()?;
    // The fourth part is whatever precedes `/containers/`, possibly empty, but it must exist.
    parts.next()?;
    let known_file = matches!(file, "hostname" | "hosts" | "resolv.conf");
    (known_file && containers == "containers")
        .then(|| ContainerId::parse(id).ok())
        .flatten()
}

/// The daemon's output, copied to the console by a task until the stream ends.
struct Pump(Option<JoinHandle<Result<(), DockerError>>>);

impl Pump {
    fn spawn(mut output: OutputStream, console: Console) -> Self {
        Self(Some(tokio::spawn(async move {
            while let Some(piece) = output.recv().await {
                console.write(piece?).await;
            }
            Ok(())
        })))
    }

    fn running(&self) -> bool {
        self.0.is_some()
    }

    /// The stream's end: `Ok` at its end, the error that broke it otherwise.
    async fn ended(&mut self) -> Result<(), DockerError> {
        let Some(task) = self.0.as_mut() else {
            return Ok(());
        };
        let ended = task.await.unwrap_or_else(|error| {
            Err(DockerError::Other(format!("the log copy failed: {error}")))
        });
        self.0 = None;
        ended
    }

    /// Waits for the last of the output, until `deadline`.
    async fn drain(&mut self, deadline: Instant) {
        if !self.running() {
            return;
        }
        match timeout_at(deadline, self.ended()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "the daemon's log stream ended with an error"),
            Err(_elapsed) => {
                tracing::warn!("the daemon's log stream was not drained in time");
                if let Some(task) = self.0.take() {
                    task.abort();
                }
            }
        }
    }
}

impl<Docker: DockerApi> Launch<'_, Docker> {
    /// Runs the launch to its end: the daemon's exit status, or 0 when stopped before the daemon
    /// started.
    ///
    /// # Errors
    ///
    /// A [`LaunchError`] for anything that refused or failed; the daemon, if created, is removed.
    pub async fn run(mut self) -> Result<u8, LaunchError> {
        let (image, denied) = self.identify().await?;
        let daemon = self.create_daemon(&image, &denied).await?;
        tracing::info!(daemon = short(&daemon), "created the daemon");
        if *self.shutdown.borrow() {
            tracing::info!(
                daemon = short(&daemon),
                "stop requested before the daemon started; removing it"
            );
            self.remove(&daemon).await?;
            return Ok(0);
        }
        let (wait, pump) = match self.start(&daemon).await {
            Ok(started) => started,
            Err(error) => {
                self.abandon(&daemon).await;
                return Err(error);
            }
        };
        tracing::info!(
            daemon = short(&daemon),
            image = image.id(),
            "started the daemon"
        );
        self.supervise(&daemon, wait, pump).await
    }

    /// The launcher's image, checked against the configuration, and the host paths no daemon may
    /// overlap: the engine's data root and the host socket.
    async fn identify(&self) -> Result<(ImageIdentity, Vec<AbsolutePath>), LaunchError> {
        let me = self
            .docker
            .inspect_container(self.launcher.as_str())
            .await
            .map_err(|source| docker_error("inspect the launcher container", source))?;
        let image_id = me
            .image
            .clone()
            .ok_or_else(|| LaunchError::SelfId("the launcher container has no image".to_owned()))?;
        let image = self
            .docker
            .inspect_image(&image_id)
            .await
            .map_err(|source| docker_error("inspect the launcher image", source))?;
        let image = ImageIdentity::new(&image.id, &image.labels)?;
        image.check_ranges(self.config)?;
        let root = self
            .docker
            .docker_root()
            .await
            .map_err(|source| docker_error("read the engine's data root", source))?;
        let denied = vec![
            host_path("the engine's data root", &root)?,
            host_path("the host socket", &socket_source(&me)?)?,
        ];
        Ok((image, denied))
    }

    /// The locked section: survey, probe, check, create. Never cancelled; the lock is released
    /// whatever happens, and a daemon created under a lock that cannot be released is removed.
    async fn create_daemon(
        &self,
        image: &ImageIdentity,
        denied: &[AbsolutePath],
    ) -> Result<String, LaunchError> {
        let lock = NodeLock::acquire(self.docker, image, &self.launcher).await?;
        let created = self.create_locked(image, denied).await;
        let released = lock.release(self.docker).await;
        match (created, released) {
            (Ok(daemon), Ok(())) => Ok(daemon),
            (Ok(daemon), Err(error)) => {
                self.abandon(&daemon).await;
                Err(error.into())
            }
            (Err(error), released) => {
                if let Err(release_error) = released {
                    tracing::error!(error = %release_error, "the node lock was not released");
                }
                Err(error)
            }
        }
    }

    async fn create_locked(
        &self,
        image: &ImageIdentity,
        denied: &[AbsolutePath],
    ) -> Result<String, LaunchError> {
        let reservations = node::survey(self.docker, self.config, &self.launcher).await?;
        let network_id = node::resolve_alias_network(self.docker, self.config).await?;
        let storage = probe::run(
            self.docker,
            self.config,
            image,
            &self.launcher,
            denied,
            self.timing.probe,
        )
        .await?;
        node::check_conflicts(self.config, network_id.as_deref(), &storage, &reservations)?;
        let spec = spec::daemon_spec(self.config, image, &self.launcher, &storage)?;
        self.docker
            .create_container(&spec)
            .await
            .map_err(|source| docker_error("create the daemon container", source))
    }

    /// Writes the subordinate ID files, attaches, starts, then registers the wait.
    async fn start(&self, daemon: &str) -> Result<(ExitWait, Pump), LaunchError> {
        let archive = spec::subid_archive(self.config).map_err(LaunchError::Archive)?;
        self.docker
            .upload(daemon, SUBID_DIR, archive)
            .await
            .map_err(|source| docker_error("write the daemon's subordinate ID files", source))?;
        let output = self
            .docker
            .attach(daemon)
            .await
            .map_err(|source| docker_error("attach to the daemon", source))?;
        let pump = Pump::spawn(output, self.console.clone());
        self.docker
            .start_container(daemon)
            .await
            .map_err(|source| docker_error("start the daemon", source))?;
        // Only now: `not-running` answers at once for a container that has not started, and
        // answers with the exit code for one that already stopped (it is not auto-removed).
        let wait = self
            .docker
            .wait(daemon)
            .await
            .map_err(|source| docker_error("wait for the daemon", source))?;
        Ok((wait, pump))
    }

    async fn supervise(
        &mut self,
        daemon: &str,
        mut wait: ExitWait,
        mut pump: Pump,
    ) -> Result<u8, LaunchError> {
        let mut health = tokio::time::interval(self.timing.health);
        health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stoppable = true;
        loop {
            tokio::select! {
                exited = &mut wait => {
                    return match flatten(exited) {
                        Ok(code) => self.exited(daemon, code, pump).await,
                        Err(reason) => {
                            tracing::error!(%reason, "the wait for the daemon failed; stopping it");
                            self.shutdown(daemon, None, pump).await?;
                            Err(LaunchError::Wait(reason))
                        }
                    };
                }
                ended = pump.ended(), if pump.running() => {
                    if let Err(error) = ended {
                        tracing::error!(%error, "the daemon's log stream was lost; stopping it");
                        self.shutdown(daemon, Some(wait), pump).await?;
                        return Err(LaunchError::LogStream(error));
                    }
                }
                changed = self.shutdown.changed(), if stoppable => match changed {
                    Ok(()) if *self.shutdown.borrow() => {
                        let code = self.shutdown(daemon, Some(wait), pump).await?;
                        return Ok(code.map_or(EXIT_FAILURE, exit_code));
                    }
                    Ok(()) => {}
                    // Nobody can ask any more.
                    Err(_closed) => stoppable = false,
                },
                _ = health.tick() => self.check_health(daemon).await,
            }
        }
    }

    /// The daemon exited by itself: its last output, its removal, its status.
    async fn exited(&self, daemon: &str, code: i64, mut pump: Pump) -> Result<u8, LaunchError> {
        tracing::info!(daemon = short(daemon), code, "the daemon exited");
        let _ = self.status.send(Status::Stopping);
        let deadline = Instant::now() + self.timing.finish;
        // Side by side: output that does not end must not take the remove's time.
        let (removed, ()) = tokio::join!(self.remove_by(daemon, deadline), pump.drain(deadline));
        removed?;
        Ok(exit_code(code))
    }

    /// Stops the daemon within [`Timing::stop`], then removes it and drains its output within
    /// [`Timing::finish`]. The daemon's exit code when `wait` gave one.
    async fn shutdown(
        &self,
        daemon: &str,
        wait: Option<ExitWait>,
        mut pump: Pump,
    ) -> Result<Option<i64>, LaunchError> {
        let _ = self.status.send(Status::Stopping);
        tracing::info!(daemon = short(daemon), "stopping the daemon");
        let stop_secs = i32::try_from(DAEMON_STOP_TIMEOUT_SECS).unwrap_or(i32::MAX);
        let stopped = timeout_at(Instant::now() + self.timing.stop, async {
            match self.docker.stop_container(daemon, stop_secs).await {
                Ok(()) | Err(DockerError::NotFound) => {}
                Err(
                    error @ (DockerError::Conflict(_)
                    | DockerError::Other(_)
                    | DockerError::Timeout),
                ) => tracing::warn!(%error, "the daemon's stop failed"),
            }
            match wait {
                Some(wait) => Some(flatten(wait.await)),
                None => None,
            }
        })
        .await;
        let deadline = Instant::now() + self.timing.finish;
        let (removed, ()) = tokio::join!(self.remove_by(daemon, deadline), pump.drain(deadline));
        removed?;
        match stopped {
            Err(_elapsed) => Err(LaunchError::StopTimeout(self.timing.stop)),
            Ok(Some(Ok(code))) => {
                tracing::info!(daemon = short(daemon), code, "the daemon stopped");
                Ok(Some(code))
            }
            Ok(Some(Err(reason))) => Err(LaunchError::Wait(reason)),
            Ok(None) => Ok(None),
        }
    }

    async fn check_health(&self, daemon: &str) {
        let health = match self.docker.inspect_container(daemon).await {
            Ok(info) => info.health,
            Err(error) => {
                tracing::warn!(%error, "inspect the daemon's health");
                None
            }
        };
        let next = Status::Running { health };
        self.status.send_if_modified(|current| {
            if *current == next {
                return false;
            }
            tracing::info!(status = next.line(), "the daemon's status changed");
            *current = next;
            true
        });
    }

    /// Removes a container this launcher made, within one call.
    async fn remove(&self, id: &str) -> Result<(), LaunchError> {
        self.remove_by(id, Instant::now() + CALL_TIMEOUT).await
    }

    async fn remove_by(&self, id: &str, deadline: Instant) -> Result<(), LaunchError> {
        let budget = deadline.saturating_duration_since(Instant::now());
        match timeout_at(deadline, remove(self.docker, id)).await {
            Ok(removed) => removed,
            Err(_elapsed) => Err(LaunchError::RemoveTimeout(budget)),
        }
    }

    /// Removes a daemon that will not be run, logging a failure: the error that led here is the
    /// one to report, and the next launcher removes an orphan anyway.
    async fn abandon(&self, daemon: &str) {
        if let Err(error) = self.remove(daemon).await {
            tracing::error!(daemon = short(daemon), %error, "the daemon was not removed");
        }
    }
}

/// Removes a container by ID; one already gone is fine.
async fn remove(docker: &impl DockerApi, id: &str) -> Result<(), LaunchError> {
    match docker.remove_container(id).await {
        Ok(()) | Err(DockerError::NotFound) => Ok(()),
        Err(source @ (DockerError::Conflict(_) | DockerError::Other(_) | DockerError::Timeout)) => {
            Err(docker_error(
                &format!("remove container {}", short(id)),
                source,
            ))
        }
    }
}

/// The host side of the launcher's [`DOCKER_SOCKET`] mount.
fn socket_source(me: &ContainerInfo) -> Result<String, LaunchError> {
    me.mounts
        .iter()
        .find(|mount| mount.destination == DOCKER_SOCKET)
        .map(|mount| mount.source.clone())
        .ok_or_else(|| {
            LaunchError::SelfId(format!(
                "the launcher container does not mount the host socket at {DOCKER_SOCKET}"
            ))
        })
}

fn host_path(what: &str, path: &str) -> Result<AbsolutePath, LaunchError> {
    AbsolutePath::parse(path)
        .map_err(|error| LaunchError::SelfId(format!("{what} {path:?}: {error}")))
}

fn flatten(
    exited: Result<Result<i64, DockerError>, tokio::sync::oneshot::error::RecvError>,
) -> Result<i64, String> {
    match exited {
        Ok(Ok(code)) => Ok(code),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_closed) => Err("the wait ended without a status".to_owned()),
    }
}

/// The launcher's exit code for the daemon's: the same when it fits, and never 0 for a non-zero
/// one.
fn exit_code(code: i64) -> u8 {
    u8::try_from(code).unwrap_or(EXIT_FAILURE)
}

fn docker_error(action: &str, source: DockerError) -> LaunchError {
    LaunchError::Docker {
        action: action.to_owned(),
        source,
    }
}
