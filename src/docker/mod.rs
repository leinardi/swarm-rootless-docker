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

//! The Docker API calls the launcher makes, behind a trait: [`Bollard`] talks to the host socket,
//! and the tests use a recording fake.
//!
//! Only what the launcher needs crosses the trait, in its own types, so a missing field in a
//! response is decided here once (an error, never a default).

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use docker_client::attach::{self, WaitCondition};
use docker_client::bollard::models::ContainerInspectResponse;
use docker_client::bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use docker_client::{CancellationToken, Config, ErrorKind, bounded};
use tokio::sync::{mpsc, oneshot};

use crate::spec::ContainerSpec;

/// How long one Docker call may take, from the request to the end of the response body.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Output pieces buffered between the engine and the launcher's own stdout and stderr.
const OUTPUT_BUFFER: usize = 64;
/// The name of Docker's default bridge network.
const DEFAULT_BRIDGE: &str = "bridge";
/// The option that marks the default bridge, whatever its name.
const DEFAULT_BRIDGE_OPTION: &str = "com.docker.network.bridge.default_bridge";

/// A failed Docker call. Only [`DockerError::NotFound`] and [`DockerError::Conflict`] are ever
/// acted on; anything else says nothing about the container and refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DockerError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("no complete answer within {CALL_TIMEOUT:?}")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

impl From<docker_client::Error> for DockerError {
    fn from(error: docker_client::Error) -> Self {
        match error.kind() {
            ErrorKind::NotFound => Self::NotFound,
            ErrorKind::Conflict => Self::Conflict(error.to_string()),
            ErrorKind::Timeout => Self::Timeout,
            ErrorKind::Unavailable | ErrorKind::Other => Self::Other(error.to_string()),
        }
    }
}

/// A container from a list call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub id: String,
    pub labels: HashMap<String, String>,
}

/// A container from an inspect call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub id: String,
    /// The name, without Docker's leading `/`.
    pub name: String,
    pub labels: HashMap<String, String>,
    /// Whether it runs (paused and restarting count as running, as in Docker). `None` when the
    /// engine reported no state: nothing may be concluded from it.
    pub running: Option<bool>,
    /// The image ID it runs, `sha256:…`.
    pub image: Option<String>,
    /// Its healthcheck status (`starting`, `healthy`, `unhealthy`), when it has one.
    pub health: Option<String>,
    pub mounts: Vec<MountInfo>,
}

/// One of a container's mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// The host side; empty for a tmpfs.
    pub source: String,
    pub destination: String,
}

/// An image from an inspect call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub id: String,
    pub labels: HashMap<String, String>,
}

/// A network from an inspect call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInfo {
    /// The full ID: the one identity of a network that can be named by name, ID or ID prefix.
    pub id: String,
    /// Whether it is Docker's default `bridge`, which has no container DNS.
    pub default_bridge: bool,
}

/// A piece of a container's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// A container's output, as attached: closed when the container's streams end.
pub type OutputStream = mpsc::Receiver<Result<Output, DockerError>>;

/// A container's exit code, once it is not running.
pub type ExitWait = oneshot::Receiver<Result<i64, DockerError>>;

/// The Docker calls the launcher makes. Every call that names a container takes its ID, except
/// [`DockerApi::inspect_container`], which also resolves a name.
pub trait DockerApi: Sync {
    /// Lists the containers that carry `label` (`key=value`): with `all`, in every state,
    /// created-but-never-started included; without, only the running ones.
    fn list_containers(
        &self,
        label: &str,
        all: bool,
    ) -> impl Future<Output = Result<Vec<ContainerSummary>, DockerError>> + Send;

    /// Inspects a container by ID or name.
    fn inspect_container(
        &self,
        id_or_name: &str,
    ) -> impl Future<Output = Result<ContainerInfo, DockerError>> + Send;

    /// Creates a container, returning its ID; [`DockerError::Conflict`] when the name is taken.
    fn create_container(
        &self,
        spec: &ContainerSpec,
    ) -> impl Future<Output = Result<String, DockerError>> + Send;

    /// Removes a container by ID, stopping it first and removing its anonymous volumes.
    fn remove_container(&self, id: &str) -> impl Future<Output = Result<(), DockerError>> + Send;

    /// Inspects an image by ID.
    fn inspect_image(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<ImageInfo, DockerError>> + Send;

    /// Inspects a network by name, ID or ID prefix.
    fn inspect_network(
        &self,
        name_or_id: &str,
    ) -> impl Future<Output = Result<NetworkInfo, DockerError>> + Send;

    /// The engine's data root (`DockerRootDir`).
    fn docker_root(&self) -> impl Future<Output = Result<String, DockerError>> + Send;

    /// Writes the tar `archive` into directory `dir` of a container, by ID.
    fn upload(
        &self,
        id: &str,
        dir: &str,
        archive: Vec<u8>,
    ) -> impl Future<Output = Result<(), DockerError>> + Send;

    /// Attaches to a container's stdout and stderr, by ID. Only establishing the stream is
    /// bounded; the stream lasts as long as the container's output.
    fn attach(&self, id: &str) -> impl Future<Output = Result<OutputStream, DockerError>> + Send;

    /// Asks for a container's exit code once it is not running, by ID. A container that already
    /// stopped answers at once, so the wait may be asked for late.
    fn wait(&self, id: &str) -> impl Future<Output = Result<ExitWait, DockerError>> + Send;

    /// Starts a container, by ID.
    fn start_container(&self, id: &str) -> impl Future<Output = Result<(), DockerError>> + Send;

    /// Stops a container, by ID: `SIGTERM`, then `SIGKILL` after `timeout_secs`.
    fn stop_container(
        &self,
        id: &str,
        timeout_secs: i32,
    ) -> impl Future<Output = Result<(), DockerError>> + Send;
}

/// The host socket, through bollard (by way of rust-common's `docker-client`).
///
/// bollard's own timeout only covers the wait for the response headers; a body that stalls after
/// them would hang the call, and under the node lock every launcher on the node. So every call is
/// bounded as a whole.
#[derive(Debug, Clone)]
pub struct Bollard {
    client: docker_client::bollard::Docker,
    timeout: Duration,
}

impl Bollard {
    /// Connects to the Unix socket at `path`, with [`CALL_TIMEOUT`] per call. Nothing is sent
    /// until the first call.
    ///
    /// # Errors
    ///
    /// [`DockerError::Other`] when the client cannot be built.
    pub fn connect(path: &str) -> Result<Self, DockerError> {
        Self::with_timeout(path, CALL_TIMEOUT)
    }

    /// [`Bollard::connect`] with another per-call timeout.
    ///
    /// # Errors
    ///
    /// [`DockerError::Other`] when the client cannot be built.
    pub fn with_timeout(path: &str, timeout: Duration) -> Result<Self, DockerError> {
        let config = Config::unix(path).response_header_timeout(timeout);
        docker_client::connect(config)
            .map(|client| Self { client, timeout })
            .map_err(|error| DockerError::Other(error.to_string()))
    }

    async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, docker_client::bollard::errors::Error>>,
    ) -> Result<T, DockerError> {
        self.bounded_by(self.timeout, call).await
    }

    async fn bounded_by<T>(
        &self,
        timeout: Duration,
        call: impl Future<Output = Result<T, docker_client::bollard::errors::Error>>,
    ) -> Result<T, DockerError> {
        // Boxed: the engine call's future is large, and every method here awaits one, which put
        // the launcher's whole future over clippy's `large_futures` limit.
        Ok(Box::pin(bounded(timeout, call)).await?)
    }
}

impl DockerApi for Bollard {
    async fn list_containers(
        &self,
        label: &str,
        all: bool,
    ) -> Result<Vec<ContainerSummary>, DockerError> {
        let filters = HashMap::from([("label", vec![label])]);
        let options = ListContainersOptionsBuilder::new()
            .all(all)
            .filters(&filters)
            .build();
        let list = self
            .bounded(self.client.list_containers(Some(options)))
            .await?;
        list.into_iter()
            .map(|summary| {
                Ok(ContainerSummary {
                    id: summary
                        .id
                        .ok_or_else(|| missing("a listed container's ID"))?,
                    labels: summary.labels.unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn inspect_container(&self, id_or_name: &str) -> Result<ContainerInfo, DockerError> {
        let response = self
            .bounded(self.client.inspect_container(id_or_name, None))
            .await?;
        container_info(response)
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String, DockerError> {
        let options = CreateContainerOptionsBuilder::new()
            .name(&spec.name)
            .build();
        self.bounded(
            self.client
                .create_container(Some(options), spec.body.clone()),
        )
        .await
        .map(|response| response.id)
    }

    async fn remove_container(&self, id: &str) -> Result<(), DockerError> {
        let options = RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(true)
            .build();
        self.bounded(self.client.remove_container(id, Some(options)))
            .await
    }

    async fn inspect_image(&self, id: &str) -> Result<ImageInfo, DockerError> {
        let image = self.bounded(self.client.inspect_image(id)).await?;
        Ok(ImageInfo {
            id: image.id.ok_or_else(|| missing("an inspected image's ID"))?,
            labels: image
                .config
                .and_then(|config| config.labels)
                .unwrap_or_default(),
        })
    }

    async fn inspect_network(&self, name_or_id: &str) -> Result<NetworkInfo, DockerError> {
        let network = self
            .bounded(self.client.inspect_network(name_or_id, None))
            .await?;
        let default_bridge = network.name.as_deref() == Some(DEFAULT_BRIDGE)
            || network
                .options
                .as_ref()
                .and_then(|options| options.get(DEFAULT_BRIDGE_OPTION))
                .is_some_and(|value| value == "true");
        Ok(NetworkInfo {
            id: network
                .id
                .ok_or_else(|| missing("an inspected network's ID"))?,
            default_bridge,
        })
    }

    async fn docker_root(&self) -> Result<String, DockerError> {
        self.bounded(self.client.info())
            .await?
            .docker_root_dir
            .ok_or_else(|| missing("the engine's DockerRootDir"))
    }

    async fn upload(&self, id: &str, dir: &str, archive: Vec<u8>) -> Result<(), DockerError> {
        let options = UploadToContainerOptionsBuilder::new().path(dir).build();
        self.bounded(self.client.upload_to_container(
            id,
            Some(options),
            docker_client::bollard::body_full(archive.into()),
        ))
        .await
    }

    async fn attach(&self, id: &str) -> Result<OutputStream, DockerError> {
        // The pump's task ends with its receiver, and so does the one below that maps its errors;
        // nothing cancels it otherwise.
        let mut attached = attach::attach(
            &self.client,
            id,
            self.timeout,
            OUTPUT_BUFFER,
            CancellationToken::new(),
        )
        .await?;
        let (sender, receiver) = mpsc::channel(OUTPUT_BUFFER);
        tokio::spawn(async move {
            while let Some(item) = attached.recv().await {
                let output = item
                    .map(|output| match output {
                        attach::Output::Stdout(message) => Output::Stdout(message),
                        attach::Output::Stderr(message) => Output::Stderr(message),
                    })
                    .map_err(DockerError::from);
                if sender.send(output).await.is_err() {
                    break;
                }
            }
        });
        Ok(receiver)
    }

    // Nothing to await here: the request goes out from the spawned task. The condition answers
    // for a container that already stopped, so the late request loses nothing.
    fn wait(&self, id: &str) -> impl Future<Output = Result<ExitWait, DockerError>> + Send {
        let mut exit = attach::wait(
            &self.client,
            id,
            WaitCondition::NotRunning,
            CancellationToken::new(),
        );
        let (mut sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            // Dropping the answer's receiver drops `exit`, which ends the wait's own task.
            let result = tokio::select! {
                biased;
                () = sender.closed() => return,
                result = &mut exit => result,
            };
            let _ = sender.send(match result {
                Ok(code) => code.map_err(DockerError::from),
                Err(_closed) => Err(DockerError::Other("the wait ended empty".to_owned())),
            });
        });
        std::future::ready(Ok(receiver))
    }

    async fn start_container(&self, id: &str) -> Result<(), DockerError> {
        self.bounded(self.client.start_container(id, None)).await
    }

    async fn stop_container(&self, id: &str, timeout_secs: i32) -> Result<(), DockerError> {
        let options = StopContainerOptionsBuilder::new().t(timeout_secs).build();
        // The engine answers once the container stopped: allow for the stop timeout itself.
        let budget = self.timeout + Duration::from_secs(u64::try_from(timeout_secs).unwrap_or(0));
        self.bounded_by(budget, self.client.stop_container(id, Some(options)))
            .await
    }
}

fn container_info(response: ContainerInspectResponse) -> Result<ContainerInfo, DockerError> {
    let id = response
        .id
        .ok_or_else(|| missing("an inspected container's ID"))?;
    let name = response
        .name
        .ok_or_else(|| missing("an inspected container's name"))?;
    Ok(ContainerInfo {
        id,
        name: name.strip_prefix('/').unwrap_or(&name).to_owned(),
        labels: response
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default(),
        running: response.state.as_ref().and_then(|state| state.running),
        image: response.image,
        health: response
            .state
            .and_then(|state| state.health)
            .and_then(|health| health.status)
            .map(|status| status.to_string())
            .filter(|status| !status.is_empty()),
        mounts: response
            .mounts
            .unwrap_or_default()
            .into_iter()
            .map(|mount| MountInfo {
                source: mount.source.unwrap_or_default(),
                destination: mount.destination.unwrap_or_default(),
            })
            .collect(),
    })
}

fn missing(what: &str) -> DockerError {
    DockerError::Other(format!("the engine returned {what} as null"))
}
