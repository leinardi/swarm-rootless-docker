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

use bollard::errors::Error as BollardError;
use bollard::models::ContainerInspectResponse;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
};

use crate::spec::ContainerSpec;

/// How long one Docker call may take, from the request to the end of the response body.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

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
}

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
}

/// The host socket, through bollard.
///
/// bollard's own timeout only covers the wait for the response headers; a body that stalls after
/// them would hang the call, and under the node lock every launcher on the node. So every call is
/// bounded as a whole.
#[derive(Debug, Clone)]
pub struct Bollard {
    client: bollard::Docker,
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
        let header_secs = timeout.as_secs().max(1);
        bollard::Docker::connect_with_unix(path, header_secs, bollard::API_DEFAULT_VERSION)
            .map(|client| Self { client, timeout })
            .map_err(|error| DockerError::Other(error.to_string()))
    }

    async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, BollardError>>,
    ) -> Result<T, DockerError> {
        match tokio::time::timeout(self.timeout, call).await {
            Ok(result) => result.map_err(|error| docker_error(&error)),
            Err(_elapsed) => Err(DockerError::Timeout),
        }
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
        running: response.state.and_then(|state| state.running),
    })
}

fn docker_error(error: &BollardError) -> DockerError {
    if let BollardError::DockerResponseServerError {
        status_code,
        message,
    } = error
    {
        match status_code {
            404 => return DockerError::NotFound,
            409 => return DockerError::Conflict(message.clone()),
            _ => {}
        }
    }
    DockerError::Other(error.to_string())
}

fn missing(what: &str) -> DockerError {
    DockerError::Other(format!("the engine returned {what} as null"))
}
