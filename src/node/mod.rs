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

//! What the launchers of one node share: the node lock, the containers a dead launcher left
//! behind, and the reservations of the daemons that still have a live owner.
//!
//! Under the lock, a launcher lists every daemon (in any state: one created but not yet started
//! is exactly what the previous lock holder leaves), removes those whose owner is provably gone,
//! checks its own configuration against the rest, creates its daemon and only then releases the
//! lock. So no two daemons on a node ever hold overlapping ranges or host paths, nor the same
//! alias on one network: this node's daemons only, the check cannot see another node's.
//!
//! A container is removed only when its owner is confirmed gone (not found, or found and not
//! running) or is this very launcher; any doubt refuses and leaves it alone. Removal is by ID,
//! never by name.

#[cfg(test)]
mod tests;

use std::hash::{BuildHasher as _, RandomState};
use std::time::Duration;

use tokio::time::Instant;

use crate::config::{LaunchConfig, NetworkName};
use crate::docker::{ContainerSummary, DockerApi, DockerError, NetworkInfo};
use crate::spec::{
    self, ContainerId, DaemonReservation, ImageIdentity, LABEL_LAUNCHER, LABEL_ROLE, LOCK_NAME,
    LabelError, ROLE_DAEMON, ROLE_LOCK, ROLE_PROBE, StorageIds,
};

/// How long a launcher waits for a node lock held by a running launcher before it gives up and
/// lets Swarm retry.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);

const BACKOFF_MIN_MILLIS: u64 = 200;
const BACKOFF_MAX_MILLIS: u64 = 1000;
const SHORT_ID_LEN: usize = 12;

/// Why the launcher cannot go on. Every one refuses; none is retried here except a lock held by
/// a running launcher, and that only until [`LOCK_WAIT`].
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("{action}: {source}")]
    Docker { action: String, source: DockerError },
    #[error("the node lock is held by running launcher {owner} (waited {LOCK_WAIT:?})")]
    LockBusy { owner: String },
    #[error("container {container} is named {LOCK_NAME} but is not a node lock; remove it by hand")]
    LockForeign { container: String },
    #[error("container {container} ({role}) has no valid {LABEL_LAUNCHER} label")]
    NoOwner { container: String, role: String },
    #[error("launcher {owner} of container {container}: inspect reported no state")]
    OwnerState { container: String, owner: String },
    #[error(
        "instance {instance} already runs: daemon {container}, owned by running launcher {owner}"
    )]
    InstanceRunning {
        instance: String,
        container: String,
        owner: String,
    },
    #[error("daemon {container}: {source}")]
    Labels {
        container: String,
        source: LabelError,
    },
    #[error("--alias needs a user-defined network: --network {network} is the default bridge")]
    AliasOnDefaultBridge { network: String },
    #[error("{what} conflicts with daemon {container} (instance {instance}): {detail}")]
    Conflict {
        what: &'static str,
        container: String,
        instance: String,
        detail: String,
    },
}

/// A daemon of another instance, with a live owner: what this one must not overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub container: String,
    pub held: DaemonReservation,
    /// The full ID its network resolves to now. Resolved only when both this launcher and that
    /// daemon have aliases; `None` also when the network is not on this node.
    pub network_id: Option<String>,
}

/// The node lock, held: a created, never started container named [`LOCK_NAME`]. Container names
/// are unique per engine, so the create is the test-and-set.
#[derive(Debug)]
#[must_use = "a held lock must be released"]
pub struct NodeLock {
    id: String,
}

impl NodeLock {
    /// Takes the lock. A lock whose holder is gone, or is this launcher, is removed and the
    /// create retried; one held by a running launcher is waited for, with jittered backoff, up to
    /// [`LOCK_WAIT`].
    ///
    /// # Errors
    ///
    /// [`NodeError::LockBusy`] after the wait; [`NodeError::LockForeign`] when the name belongs
    /// to something else; an ownership or Docker error otherwise.
    pub async fn acquire(
        docker: &impl DockerApi,
        image: &ImageIdentity,
        launcher: &ContainerId,
    ) -> Result<Self, NodeError> {
        let lock = spec::lock_spec(image, launcher);
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match docker.create_container(&lock).await {
                Ok(id) => return Ok(Self { id }),
                Err(DockerError::Conflict(_)) => {}
                Err(
                    error @ (DockerError::NotFound | DockerError::Other(_) | DockerError::Timeout),
                ) => {
                    return Err(docker_error("create the node lock", error));
                }
            }
            let holder = match docker.inspect_container(LOCK_NAME).await {
                Ok(holder) => holder,
                // Released between the create and the inspect: try again.
                Err(DockerError::NotFound) => continue,
                Err(
                    error @ (DockerError::Conflict(_)
                    | DockerError::Other(_)
                    | DockerError::Timeout),
                ) => {
                    return Err(docker_error("inspect the node lock", error));
                }
            };
            if holder.labels.get(LABEL_ROLE).map(String::as_str) != Some(ROLE_LOCK) {
                return Err(NodeError::LockForeign {
                    container: short(&holder.id),
                });
            }
            match owner_of(docker, &holder.id, &holder.labels, launcher).await? {
                Owner::Myself | Owner::Gone => {
                    tracing::info!(lock = short(&holder.id), "removing a stale node lock");
                    remove(docker, &holder.id).await?;
                }
                Owner::Running(owner) => {
                    if Instant::now() >= deadline {
                        return Err(NodeError::LockBusy {
                            owner: short(&owner),
                        });
                    }
                    tracing::debug!(owner = short(&owner), "node lock held, waiting");
                    tokio::time::sleep(backoff()).await;
                }
            }
        }
    }

    /// Releases the lock, by ID.
    ///
    /// # Errors
    ///
    /// A Docker error other than not found. Not found means another launcher judged this one
    /// gone and removed the lock: logged, since the guarantee no longer held.
    pub async fn release(self, docker: &impl DockerApi) -> Result<(), NodeError> {
        match docker.remove_container(&self.id).await {
            Ok(()) => Ok(()),
            Err(DockerError::NotFound) => {
                tracing::warn!(lock = short(&self.id), "the node lock was already removed");
                Ok(())
            }
            Err(
                error @ (DockerError::Conflict(_) | DockerError::Other(_) | DockerError::Timeout),
            ) => Err(docker_error("release the node lock", error)),
        }
    }
}

/// Takes the node lock, runs `work` under it and releases it, whether `work` succeeded or not.
/// An error from `work` wins over one from the release.
///
/// # Errors
///
/// The error of the acquire, of `work`, or of the release.
pub async fn with_node_lock<T, E: From<NodeError>>(
    docker: &impl DockerApi,
    image: &ImageIdentity,
    launcher: &ContainerId,
    work: impl AsyncFnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let lock = NodeLock::acquire(docker, image, launcher).await?;
    let result = work().await;
    let released = lock.release(docker).await;
    let value = result?;
    released?;
    Ok(value)
}

/// Under the lock: removes the probes and daemons whose owner is gone (or is this launcher), and
/// returns the daemons of other instances that keep a live owner.
///
/// # Errors
///
/// [`NodeError::InstanceRunning`] when a live launcher already runs this instance; an ownership,
/// label or Docker error for any container it cannot judge.
pub async fn survey(
    docker: &impl DockerApi,
    config: &LaunchConfig,
    launcher: &ContainerId,
) -> Result<Vec<Reservation>, NodeError> {
    for probe in list_role(docker, ROLE_PROBE).await? {
        match owner_of(docker, &probe.id, &probe.labels, launcher).await? {
            Owner::Myself | Owner::Gone => {
                tracing::info!(probe = short(&probe.id), "removing a stale probe");
                remove(docker, &probe.id).await?;
            }
            Owner::Running(_) => {}
        }
    }
    let mut reservations = Vec::new();
    for daemon in list_role(docker, ROLE_DAEMON).await? {
        let owner = match owner_of(docker, &daemon.id, &daemon.labels, launcher).await? {
            Owner::Myself => {
                tracing::info!(
                    daemon = short(&daemon.id),
                    "removing a daemon left by an earlier run of this launcher"
                );
                remove(docker, &daemon.id).await?;
                continue;
            }
            Owner::Gone => {
                tracing::info!(daemon = short(&daemon.id), "removing an orphaned daemon");
                remove(docker, &daemon.id).await?;
                continue;
            }
            Owner::Running(owner) => owner,
        };
        let held =
            DaemonReservation::from_labels(&daemon.labels).map_err(|source| NodeError::Labels {
                container: short(&daemon.id),
                source,
            })?;
        if held.instance == *config.instance() {
            return Err(NodeError::InstanceRunning {
                instance: held.instance.to_string(),
                container: short(&daemon.id),
                owner: short(&owner),
            });
        }
        let network_id = match &held.network {
            Some(network) if !config.aliases().is_empty() && !held.aliases.is_empty() => {
                resolve_network(docker, network)
                    .await?
                    .map(|resolved| resolved.id)
            }
            Some(_) | None => None,
        };
        reservations.push(Reservation {
            container: daemon.id,
            held,
            network_id,
        });
    }
    Ok(reservations)
}

/// Under the lock, when `config` has aliases: the full ID of its `--network`, or `None` when the
/// network is not on this node yet (an attachable overlay no container here has joined). Only
/// aliases can make two daemons' names meet: container names, and so hostnames, differ by
/// instance, under a prefix no alias may take. A name,
/// an ID and an ID prefix of one network all resolve to the same ID.
///
/// # Errors
///
/// [`NodeError::AliasOnDefaultBridge`] when the network is the default bridge, however named; a
/// Docker error other than not found.
pub async fn resolve_alias_network(
    docker: &impl DockerApi,
    config: &LaunchConfig,
) -> Result<Option<String>, NodeError> {
    if config.aliases().is_empty() {
        return Ok(None);
    }
    match resolve_network(docker, config.network()).await? {
        Some(resolved) if resolved.default_bridge => Err(NodeError::AliasOnDefaultBridge {
            network: config.network().to_string(),
        }),
        resolved => Ok(resolved.map(|resolved| resolved.id)),
    }
}

/// Checks `config`, with the identities the probe found for its host paths and the ID of its
/// network ([`resolve_alias_network`]), against every reservation: no overlapping subordinate
/// range, no host path that is the same as, inside or around one of theirs, by path or by
/// physical location (so through a bind mount too), and no DNS name of theirs on the same network
/// ([`spec::dns_names`]: container name, which is also the hostname, and aliases).
///
/// Two networks count as different only when both resolved to different IDs: one that did not
/// resolve may be the other. A daemon live on a network is attached to it on this node, so its
/// network resolves, and so does the candidate's when it names the same one.
///
/// # Errors
///
/// [`NodeError::Conflict`] for the first overlap found.
pub fn check_conflicts(
    config: &LaunchConfig,
    network_id: Option<&str>,
    storage: &StorageIds,
    reservations: &[Reservation],
) -> Result<(), NodeError> {
    let candidate = storage.entries(config);
    for reservation in reservations {
        let held = &reservation.held;
        let conflict = |what, detail: String| NodeError::Conflict {
            what,
            container: short(&reservation.container),
            instance: held.instance.to_string(),
            detail,
        };
        if config.subuid().overlaps(held.subuid) {
            return Err(conflict(
                "--subuid",
                format!("{} overlaps {}", config.subuid(), held.subuid),
            ));
        }
        if config.subgid().overlaps(held.subgid) {
            return Err(conflict(
                "--subgid",
                format!("{} overlaps {}", config.subgid(), held.subgid),
            ));
        }
        for (path, location) in &candidate {
            for (held_path, held_location) in &held.storage {
                if path.overlaps(held_path.as_str()) {
                    return Err(conflict(
                        "host path",
                        format!("{path} overlaps {held_path}"),
                    ));
                }
                if location.overlaps(held_location) {
                    return Err(conflict(
                        "host path",
                        format!(
                            "{path} and {held_path} share storage on the host \
                             (through a bind mount, a hard link or a second path)"
                        ),
                    ));
                }
            }
        }
        let same_network = match (network_id, reservation.network_id.as_deref()) {
            (Some(mine), Some(theirs)) => mine == theirs,
            (Some(_) | None, None) | (None, Some(_)) => true,
        };
        let theirs = spec::dns_names(&held.instance, &held.aliases);
        if let Some(name) = spec::dns_names(config.instance(), config.aliases())
            .into_iter()
            .find(|name| theirs.contains(name))
            && same_network
        {
            let network = held
                .network
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            return Err(conflict(
                "--alias",
                format!("{name} is also one of its names on network {network}"),
            ));
        }
    }
    Ok(())
}

/// A network by name, ID or ID prefix; `None` when the engine does not know it.
async fn resolve_network(
    docker: &impl DockerApi,
    network: &NetworkName,
) -> Result<Option<NetworkInfo>, NodeError> {
    match docker.inspect_network(network.as_str()).await {
        Ok(resolved) => Ok(Some(resolved)),
        Err(DockerError::NotFound) => Ok(None),
        Err(error @ (DockerError::Conflict(_) | DockerError::Other(_) | DockerError::Timeout)) => {
            Err(docker_error(&format!("inspect network {network}"), error))
        }
    }
}

/// Who owns a container, as far as removal is concerned.
enum Owner {
    /// This launcher: a container it made before a restart.
    Myself,
    /// Confirmed not found, or found and not running.
    Gone,
    /// A running launcher, by ID.
    Running(String),
}

async fn owner_of(
    docker: &impl DockerApi,
    container: &str,
    labels: &std::collections::HashMap<String, String>,
    launcher: &ContainerId,
) -> Result<Owner, NodeError> {
    let no_owner = || NodeError::NoOwner {
        container: short(container),
        role: labels.get(LABEL_ROLE).cloned().unwrap_or_default(),
    };
    let owner = labels.get(LABEL_LAUNCHER).ok_or_else(no_owner)?;
    let owner = ContainerId::parse(owner).map_err(|_| no_owner())?;
    if owner == *launcher {
        return Ok(Owner::Myself);
    }
    match docker.inspect_container(owner.as_str()).await {
        Err(DockerError::NotFound) => Ok(Owner::Gone),
        // A timeout or an engine error says nothing about whether the owner still runs; only a
        // confirmed absence makes the container an orphan.
        Err(error @ (DockerError::Conflict(_) | DockerError::Other(_) | DockerError::Timeout)) => {
            Err(docker_error(
                &format!(
                    "inspect launcher {} of {}",
                    short(owner.as_str()),
                    short(container)
                ),
                error,
            ))
        }
        Ok(info) => match info.running {
            None => Err(NodeError::OwnerState {
                container: short(container),
                owner: short(owner.as_str()),
            }),
            Some(true) => Ok(Owner::Running(info.id)),
            Some(false) => Ok(Owner::Gone),
        },
    }
}

/// Lists the containers of `role`, always in every state: a created, never started container is a
/// reservation too.
async fn list_role(
    docker: &impl DockerApi,
    role: &str,
) -> Result<Vec<ContainerSummary>, NodeError> {
    docker
        .list_containers(&format!("{LABEL_ROLE}={role}"), true)
        .await
        .map_err(|error| docker_error(&format!("list the {role} containers"), error))
}

/// Removes a container by ID; one already gone is fine.
async fn remove(docker: &impl DockerApi, id: &str) -> Result<(), NodeError> {
    match docker.remove_container(id).await {
        Ok(()) | Err(DockerError::NotFound) => Ok(()),
        Err(error @ (DockerError::Conflict(_) | DockerError::Other(_) | DockerError::Timeout)) => {
            Err(docker_error(
                &format!("remove container {}", short(id)),
                error,
            ))
        }
    }
}

fn docker_error(action: &str, source: DockerError) -> NodeError {
    NodeError::Docker {
        action: action.to_owned(),
        source,
    }
}

/// A random wait between the backoff bounds, so launchers that collided do not collide again.
fn backoff() -> Duration {
    let random = RandomState::new().hash_one(Instant::now());
    Duration::from_millis(BACKOFF_MIN_MILLIS + random % (BACKOFF_MAX_MILLIS - BACKOFF_MIN_MILLIS))
}

/// A container ID shortened as Docker prints it, for logs and messages.
pub(crate) fn short(id: &str) -> String {
    id.chars().take(SHORT_ID_LEN).collect()
}
