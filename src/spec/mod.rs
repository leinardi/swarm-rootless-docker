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

//! The containers the launcher creates, each from one function with no options: the daemon, the
//! host-side probe and the node lock. Nothing in a compose file can widen them; the flags only
//! pick the instance, its ranges, its host paths, its network and its limits.
//!
//! The daemon's profile is the smallest that runs rootless dockerd unprivileged: all capabilities
//! dropped except `SETUID` and `SETGID` (for `newuidmap` and `newgidmap`, which work through file
//! capabilities, so `no-new-privileges` must stay off), `seccomp`, `apparmor` and the system
//! paths unconfined (no masked or read-only paths: rootlesskit mounts its own `/proc` and `/sys`), and `/dev/net/tun` for its network. Never `--privileged`, never a host namespace, never the host socket.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;

use bollard::models::{
    ContainerCreateBody, DeviceMapping, HealthConfig, HostConfig, HostConfigCgroupnsModeEnum,
    HostConfigLogConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum,
    MountTmpfsOptions, MountType, RestartPolicy, RestartPolicyNameEnum,
};
use serde::Serialize;

use crate::config::{IdRange, LaunchConfig, NanoCpus};

/// The prefix of every container name and label key.
pub const PREFIX: &str = "swarm-rootless-docker";
/// The name of the node lock. The daemon and probe names carry a `-daemon-` or `-probe-` infix,
/// so no instance name can produce it, nor another role's name.
pub const LOCK_NAME: &str = "swarm-rootless-docker-lock";
/// The binary inside the image.
pub const BINARY: &str = "/usr/local/bin/swarm-rootless-docker";

/// The subcommand the daemon container runs.
pub const DAEMON_COMMAND: &str = "daemon";
/// The subcommand of the daemon container's healthcheck.
pub const DAEMON_HEALTH_COMMAND: &str = "daemon-health";
/// The subcommand the probe container runs.
pub const PROBE_COMMAND: &str = "probe";

/// Where the data root is mounted: the rootless daemon's default data root, the first upstream
/// `VOLUME`.
pub const DATA_ROOT_TARGET: &str = "/home/rootless/.local/share/docker";
/// Where the socket directory is mounted.
pub const SOCKET_DIR_TARGET: &str = "/home/rootless/socket";
/// The daemon's only listener, inside the socket directory.
pub const SOCKET_PATH: &str = "/home/rootless/socket/docker.sock";
/// The second upstream `VOLUME`, unused by a rootless daemon; a tmpfs keeps Docker from creating
/// an anonymous volume for it.
pub const VAR_LIB_DOCKER: &str = "/var/lib/docker";
/// The parent of `XDG_RUNTIME_DIR` (`/run/user/<uid>`), where rootlesskit keeps its state.
pub const RUN_USER: &str = "/run/user";
/// Where the probe sees the host's `/`.
pub const PROBE_HOST_ROOT: &str = "/host";

/// The image label holding the daemon's UID.
pub const LABEL_IMAGE_UID: &str = "io.github.leinardi.swarm-rootless-docker.uid";
/// The image label holding the daemon's GID.
pub const LABEL_IMAGE_GID: &str = "io.github.leinardi.swarm-rootless-docker.gid";
/// Which of the launcher's containers this is: [`ROLE_DAEMON`], [`ROLE_PROBE`] or [`ROLE_LOCK`].
pub const LABEL_ROLE: &str = "io.github.leinardi.swarm-rootless-docker.role";
/// The ID of the launcher container that owns this one.
pub const LABEL_LAUNCHER: &str = "io.github.leinardi.swarm-rootless-docker.launcher";
/// The daemon's instance name.
pub const LABEL_INSTANCE: &str = "io.github.leinardi.swarm-rootless-docker.instance";
/// The daemon's subordinate UID range, `START:COUNT`.
pub const LABEL_SUBUID: &str = "io.github.leinardi.swarm-rootless-docker.subuid";
/// The daemon's subordinate GID range, `START:COUNT`.
pub const LABEL_SUBGID: &str = "io.github.leinardi.swarm-rootless-docker.subgid";
/// The daemon's host paths with their identities, as the probe saw them: a JSON array of
/// `{"path", "device", "inode"}`, data root first, then the socket directory, then each mount
/// source in flag order.
pub const LABEL_STORAGE: &str = "io.github.leinardi.swarm-rootless-docker.storage";

/// [`LABEL_ROLE`] of a daemon container.
pub const ROLE_DAEMON: &str = "daemon";
/// [`LABEL_ROLE`] of a probe container.
pub const ROLE_PROBE: &str = "probe";
/// [`LABEL_ROLE`] of the node lock.
pub const ROLE_LOCK: &str = "lock";

/// Seconds Docker waits after `SIGTERM` before it kills the daemon; dockerd stops its own
/// containers within it.
pub const DAEMON_STOP_TIMEOUT_SECS: i64 = 15;

const SECOND_NANOS: i64 = 1_000_000_000;
const MIB: i64 = 1024 * 1024;
const PROBE_MEMORY_BYTES: i64 = 64 * MIB;
const PROBE_PIDS_LIMIT: i64 = 64;
const PROBE_STOP_TIMEOUT_SECS: i64 = 1;
const TMPFS_VAR_LIB_DOCKER_BYTES: i64 = 16 * MIB;
const TMPFS_RUN_USER_BYTES: i64 = 64 * MIB;
const SUBID_FILE_MODE: u32 = 0o644;
/// The image's user, whose `/etc/subuid` and `/etc/subgid` lines the launcher writes.
const ROOTLESS_USER: &str = "rootless";
const IMAGE_ID_PREFIX: &str = "sha256:";
const ID_HEX_LEN: usize = 64;

/// A value Docker returned, or the launcher was handed, that the specs cannot be built from.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("image ID {0:?} is not sha256: and 64 lowercase hex digits")]
    ImageId(String),
    #[error("container ID {0:?} is not 64 lowercase hex digits")]
    ContainerId(String),
    #[error("image label {0} is missing")]
    LabelMissing(&'static str),
    #[error("image label {label} is {value:?}, not a decimal ID from 1 to 4294967294")]
    LabelInvalid { label: &'static str, value: String },
    #[error("--{flag} {range} contains the image's {kind} {id}")]
    RangeContainsImageId {
        flag: &'static str,
        range: IdRange,
        kind: &'static str,
        id: u32,
    },
    #[error("{expected} mount source identities were expected, {actual} were given")]
    StorageCount { expected: usize, actual: usize },
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

/// A container ID: 64 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerId(String);

impl ContainerId {
    /// Parses a full container ID.
    ///
    /// # Errors
    ///
    /// [`SpecError::ContainerId`] for anything else, a short ID included.
    pub fn parse(value: &str) -> Result<Self, SpecError> {
        if is_lower_hex(value, ID_HEX_LEN) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SpecError::ContainerId(value.to_owned()))
        }
    }

    /// The ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The launcher's own image: its content-addressed ID, which every container it creates runs, and
/// the daemon UID and GID it was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageIdentity {
    id: String,
    uid: u32,
    gid: u32,
}

impl ImageIdentity {
    /// Reads the identity from an image inspect: the ID and the [`LABEL_IMAGE_UID`] and
    /// [`LABEL_IMAGE_GID`] labels.
    ///
    /// # Errors
    ///
    /// A [`SpecError`] for an ID that is not `sha256:` and 64 hex digits, or a label that is
    /// missing, not decimal, 0 (root) or `(uid_t) -1`.
    pub fn new(id: &str, labels: &HashMap<String, String>) -> Result<Self, SpecError> {
        let digest = id
            .strip_prefix(IMAGE_ID_PREFIX)
            .ok_or_else(|| SpecError::ImageId(id.to_owned()))?;
        if !is_lower_hex(digest, ID_HEX_LEN) {
            return Err(SpecError::ImageId(id.to_owned()));
        }
        Ok(Self {
            id: id.to_owned(),
            uid: image_id_label(labels, LABEL_IMAGE_UID)?,
            gid: image_id_label(labels, LABEL_IMAGE_GID)?,
        })
    }

    /// The image ID, `sha256:…`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The daemon's UID.
    #[must_use]
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// The daemon's GID.
    #[must_use]
    pub fn gid(&self) -> u32 {
        self.gid
    }

    /// Checks that neither subordinate range contains the daemon's own ID: inner IDs would then
    /// map onto the daemon user, and so onto the consumer.
    ///
    /// # Errors
    ///
    /// [`SpecError::RangeContainsImageId`] naming the flag.
    pub fn check_ranges(&self, config: &LaunchConfig) -> Result<(), SpecError> {
        if config.subuid().contains(self.uid) {
            return Err(SpecError::RangeContainsImageId {
                flag: "subuid",
                range: config.subuid(),
                kind: "UID",
                id: self.uid,
            });
        }
        if config.subgid().contains(self.gid) {
            return Err(SpecError::RangeContainsImageId {
                flag: "subgid",
                range: config.subgid(),
                kind: "GID",
                id: self.gid,
            });
        }
        Ok(())
    }
}

/// A host file's identity, `st_dev` and `st_ino`: two paths with the same one are the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

/// The identities the probe found for a configuration's host paths, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageIds {
    data_root: FileId,
    socket_dir: FileId,
    mounts: Vec<FileId>,
}

impl StorageIds {
    /// Pairs the identities with `config`'s paths: the data root, the socket directory, then one
    /// per mount source, in flag order.
    ///
    /// # Errors
    ///
    /// [`SpecError::StorageCount`] when `mounts` does not have one identity per mount.
    pub fn new(
        config: &LaunchConfig,
        data_root: FileId,
        socket_dir: FileId,
        mounts: Vec<FileId>,
    ) -> Result<Self, SpecError> {
        if mounts.len() != config.mounts().len() {
            return Err(SpecError::StorageCount {
                expected: config.mounts().len(),
                actual: mounts.len(),
            });
        }
        Ok(Self {
            data_root,
            socket_dir,
            mounts,
        })
    }
}

/// A container to create: its name and the create request.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSpec {
    pub name: String,
    pub body: ContainerCreateBody,
}

/// The daemon container's name for an instance.
#[must_use]
pub fn daemon_name(config: &LaunchConfig) -> String {
    format!("{PREFIX}-daemon-{}", config.instance())
}

/// The probe container's name for an instance.
#[must_use]
pub fn probe_name(config: &LaunchConfig) -> String {
    format!("{PREFIX}-probe-{}", config.instance())
}

/// The labels of a daemon container: what the next launcher on the node reads to find who owns
/// it and which ranges and host paths it holds.
///
/// # Errors
///
/// [`SpecError::Encode`], which a list of paths and numbers cannot produce.
pub fn daemon_labels(
    config: &LaunchConfig,
    launcher: &ContainerId,
    storage: &StorageIds,
) -> Result<HashMap<String, String>, SpecError> {
    #[derive(Serialize)]
    struct Entry<'path> {
        path: &'path str,
        #[serde(flatten)]
        id: FileId,
    }
    let mut entries = vec![
        Entry {
            path: config.data_root().path().as_str(),
            id: storage.data_root,
        },
        Entry {
            path: config.socket_dir().path().as_str(),
            id: storage.socket_dir,
        },
    ];
    entries.extend(
        config
            .mounts()
            .iter()
            .zip(&storage.mounts)
            .map(|(mount, id)| Entry {
                path: mount.source().path().as_str(),
                id: *id,
            }),
    );
    let mut labels = owned_labels(ROLE_DAEMON, launcher);
    labels.extend([
        (LABEL_INSTANCE.to_owned(), config.instance().to_string()),
        (LABEL_SUBUID.to_owned(), config.subuid().to_string()),
        (LABEL_SUBGID.to_owned(), config.subgid().to_string()),
        (LABEL_STORAGE.to_owned(), serde_json::to_string(&entries)?),
    ]);
    Ok(labels)
}

/// The daemon container: the launcher's image running `daemon` as the image's UID and GID, with
/// the fixed profile, the configured mounts, network and limits, and no restart policy (the
/// launcher, and through it Swarm, owns the lifecycle).
///
/// `AutoRemove` is off: the launcher waits for `not-running` and removes the container itself, so
/// an early exit cannot remove it before the wait is registered.
///
/// # Errors
///
/// [`SpecError::Encode`] from [`daemon_labels`].
pub fn daemon_spec(
    config: &LaunchConfig,
    image: &ImageIdentity,
    launcher: &ContainerId,
    storage: &StorageIds,
) -> Result<ContainerSpec, SpecError> {
    let mut mounts = vec![
        bind(config.data_root().path().as_str(), DATA_ROOT_TARGET, false),
        bind(
            config.socket_dir().path().as_str(),
            SOCKET_DIR_TARGET,
            false,
        ),
    ];
    mounts.extend(config.mounts().iter().map(|mount| {
        bind(
            mount.source().path().as_str(),
            mount.target().path().as_str(),
            mount.read_only(),
        )
    }));
    mounts.push(tmpfs(VAR_LIB_DOCKER, TMPFS_VAR_LIB_DOCKER_BYTES, 0o755));
    mounts.push(tmpfs(RUN_USER, TMPFS_RUN_USER_BYTES, 0o1777));

    let body = ContainerCreateBody {
        image: Some(image.id.clone()),
        entrypoint: Some(vec![BINARY.to_owned()]),
        cmd: Some(vec![DAEMON_COMMAND.to_owned()]),
        user: Some(format!("{}:{}", image.uid, image.gid)),
        labels: Some(daemon_labels(config, launcher, storage)?),
        healthcheck: Some(HealthConfig {
            test: Some(vec![
                "CMD".to_owned(),
                BINARY.to_owned(),
                DAEMON_HEALTH_COMMAND.to_owned(),
            ]),
            interval: Some(10 * SECOND_NANOS),
            timeout: Some(5 * SECOND_NANOS),
            start_period: Some(60 * SECOND_NANOS),
            retries: Some(3),
            ..HealthConfig::default()
        }),
        stop_signal: Some("SIGTERM".to_owned()),
        stop_timeout: Some(DAEMON_STOP_TIMEOUT_SECS),
        host_config: Some(HostConfig {
            privileged: Some(false),
            cap_drop: Some(vec!["ALL".to_owned()]),
            cap_add: Some(vec!["SETUID".to_owned(), "SETGID".to_owned()]),
            security_opt: Some(vec![
                "seccomp=unconfined".to_owned(),
                "apparmor=unconfined".to_owned(),
            ]),
            // `--security-opt systempaths=unconfined` is the CLI's spelling; the API takes empty
            // lists (an absent one means Docker's defaults) and refuses the option itself.
            masked_paths: Some(Vec::new()),
            readonly_paths: Some(Vec::new()),
            devices: Some(vec![DeviceMapping {
                path_on_host: Some("/dev/net/tun".to_owned()),
                path_in_container: Some("/dev/net/tun".to_owned()),
                cgroup_permissions: Some("rwm".to_owned()),
            }]),
            ipc_mode: Some("private".to_owned()),
            cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
            network_mode: Some(config.network().as_str().to_owned()),
            readonly_rootfs: Some(false),
            init: Some(false),
            restart_policy: Some(no_restart()),
            auto_remove: Some(false),
            mounts: Some(mounts),
            memory: Some(config.memory().bytes()),
            memory_swap: Some(config.memory().bytes()),
            pids_limit: Some(config.pids_limit().get()),
            nano_cpus: config.cpus().map(NanoCpus::get),
            log_config: Some(HostConfigLogConfig {
                typ: Some("local".to_owned()),
                config: Some(HashMap::from([
                    ("max-size".to_owned(), "10m".to_owned()),
                    ("max-file".to_owned(), "3".to_owned()),
                ])),
            }),
            ..HostConfig::default()
        }),
        ..ContainerCreateBody::default()
    };
    Ok(ContainerSpec {
        name: daemon_name(config),
        body,
    })
}

/// What the probe checks on the host: the paths the daemon will get and the ranges it will map.
/// Passed to the probe container as one JSON argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeRequest {
    data_root: String,
    socket_dir: String,
    mount_sources: Vec<String>,
    owner_uid: u32,
    subuid: Range,
    subgid: Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct Range {
    start: u32,
    count: u32,
}

impl From<IdRange> for Range {
    fn from(range: IdRange) -> Self {
        Self {
            start: range.start(),
            count: range.count(),
        }
    }
}

impl ProbeRequest {
    /// The request for `config`: its paths, the image UID the data root and socket directory must
    /// belong to, and its ranges.
    #[must_use]
    pub fn new(config: &LaunchConfig, image: &ImageIdentity) -> Self {
        Self {
            data_root: config.data_root().path().as_str().to_owned(),
            socket_dir: config.socket_dir().path().as_str().to_owned(),
            mount_sources: config
                .mounts()
                .iter()
                .map(|mount| mount.source().path().as_str().to_owned())
                .collect(),
            owner_uid: image.uid,
            subuid: config.subuid().into(),
            subgid: config.subgid().into(),
        }
    }
}

/// The probe container: the launcher's image running `probe` as root with only
/// `DAC_READ_SEARCH` (to walk directories it does not own), the host's `/` read-only at
/// [`PROBE_HOST_ROOT`], no network and a read-only root filesystem. It reads metadata and prints
/// one report.
///
/// The host bind sets no propagation. The source contains Docker's data root, for which Docker
/// refuses `rprivate`; left unset, Docker picks `rslave` itself and falls back to private when
/// the host's `/` is a private mount, where an explicit `rslave` would fail the start. It is
/// recursively read-only, failing instead of silently leaving a submount writable on a kernel
/// that cannot. The image healthcheck is disabled: it belongs to the
/// launcher.
///
/// # Errors
///
/// [`SpecError::Encode`], which the request cannot produce.
pub fn probe_spec(
    config: &LaunchConfig,
    image: &ImageIdentity,
    launcher: &ContainerId,
    request: &ProbeRequest,
) -> Result<ContainerSpec, SpecError> {
    let body = ContainerCreateBody {
        image: Some(image.id.clone()),
        entrypoint: Some(vec![BINARY.to_owned()]),
        cmd: Some(vec![
            PROBE_COMMAND.to_owned(),
            serde_json::to_string(request)?,
        ]),
        user: Some("0:0".to_owned()),
        labels: Some(owned_labels(ROLE_PROBE, launcher)),
        healthcheck: Some(no_healthcheck()),
        network_disabled: Some(true),
        stop_signal: Some("SIGTERM".to_owned()),
        stop_timeout: Some(PROBE_STOP_TIMEOUT_SECS),
        host_config: Some(HostConfig {
            privileged: Some(false),
            cap_drop: Some(vec!["ALL".to_owned()]),
            cap_add: Some(vec!["DAC_READ_SEARCH".to_owned()]),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            ipc_mode: Some("private".to_owned()),
            cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
            network_mode: Some("none".to_owned()),
            readonly_rootfs: Some(true),
            init: Some(false),
            restart_policy: Some(no_restart()),
            auto_remove: Some(false),
            mounts: Some(vec![Mount {
                target: Some(PROBE_HOST_ROOT.to_owned()),
                source: Some("/".to_owned()),
                typ: Some(MountType::BIND),
                read_only: Some(true),
                bind_options: Some(MountBindOptions {
                    read_only_force_recursive: Some(true),
                    ..MountBindOptions::default()
                }),
                ..Mount::default()
            }]),
            memory: Some(PROBE_MEMORY_BYTES),
            memory_swap: Some(PROBE_MEMORY_BYTES),
            pids_limit: Some(PROBE_PIDS_LIMIT),
            log_config: Some(no_logs()),
            ..HostConfig::default()
        }),
        ..ContainerCreateBody::default()
    };
    Ok(ContainerSpec {
        name: probe_name(config),
        body,
    })
}

/// The node lock: a container that is created and never started. Its name is the lock; its
/// labels say which launcher holds it. If it were started anyway, it would print the usage and
/// exit, unprivileged and with no network.
#[must_use]
pub fn lock_spec(image: &ImageIdentity, launcher: &ContainerId) -> ContainerSpec {
    let body = ContainerCreateBody {
        image: Some(image.id.clone()),
        entrypoint: Some(vec![BINARY.to_owned()]),
        cmd: Some(Vec::new()),
        user: Some(format!("{}:{}", image.uid, image.gid)),
        labels: Some(owned_labels(ROLE_LOCK, launcher)),
        healthcheck: Some(no_healthcheck()),
        network_disabled: Some(true),
        host_config: Some(HostConfig {
            privileged: Some(false),
            cap_drop: Some(vec!["ALL".to_owned()]),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            network_mode: Some("none".to_owned()),
            readonly_rootfs: Some(true),
            restart_policy: Some(no_restart()),
            auto_remove: Some(false),
            log_config: Some(no_logs()),
            ..HostConfig::default()
        }),
        ..ContainerCreateBody::default()
    };
    ContainerSpec {
        name: LOCK_NAME.to_owned(),
        body,
    }
}

/// The tar archive the launcher uploads into `/etc` of the created daemon container, before it
/// starts: `subuid` and `subgid`, each one `rootless:START:COUNT` line, owned by root, mode 0644.
/// The daemon user cannot change them, and they decide which host IDs it may map.
///
/// # Errors
///
/// An I/O error from the archive writer, which writing to memory does not produce.
pub fn subid_archive(config: &LaunchConfig) -> io::Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, range) in [("subuid", config.subuid()), ("subgid", config.subgid())] {
        let content = format!("{ROOTLESS_USER}:{range}\n");
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(u64::try_from(content.len()).unwrap_or(u64::MAX));
        header.set_mode(SUBID_FILE_MODE);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        archive.append_data(&mut header, name, content.as_bytes())?;
    }
    archive.into_inner()
}

fn owned_labels(role: &str, launcher: &ContainerId) -> HashMap<String, String> {
    HashMap::from([
        (LABEL_ROLE.to_owned(), role.to_owned()),
        (LABEL_LAUNCHER.to_owned(), launcher.as_str().to_owned()),
    ])
}

fn image_id_label(labels: &HashMap<String, String>, label: &'static str) -> Result<u32, SpecError> {
    let value = labels.get(label).ok_or(SpecError::LabelMissing(label))?;
    let invalid = || SpecError::LabelInvalid {
        label,
        value: value.clone(),
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    match value.parse::<u32>() {
        Ok(id) if id != 0 && id != u32::MAX => Ok(id),
        Ok(_) | Err(_) => Err(invalid()),
    }
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bind(source: &str, target: &str, read_only: bool) -> Mount {
    Mount {
        target: Some(target.to_owned()),
        source: Some(source.to_owned()),
        typ: Some(MountType::BIND),
        read_only: Some(read_only),
        bind_options: Some(MountBindOptions {
            propagation: Some(MountBindOptionsPropagationEnum::RPRIVATE),
            ..MountBindOptions::default()
        }),
        ..Mount::default()
    }
}

fn tmpfs(target: &str, size_bytes: i64, mode: i64) -> Mount {
    Mount {
        target: Some(target.to_owned()),
        typ: Some(MountType::TMPFS),
        tmpfs_options: Some(MountTmpfsOptions {
            size_bytes: Some(size_bytes),
            mode: Some(mode),
            ..MountTmpfsOptions::default()
        }),
        ..Mount::default()
    }
}

fn no_restart() -> RestartPolicy {
    RestartPolicy {
        name: Some(RestartPolicyNameEnum::NO),
        ..RestartPolicy::default()
    }
}

fn no_healthcheck() -> HealthConfig {
    HealthConfig {
        test: Some(vec!["NONE".to_owned()]),
        ..HealthConfig::default()
    }
}

fn no_logs() -> HostConfigLogConfig {
    HostConfigLogConfig {
        typ: Some("none".to_owned()),
        config: None,
    }
}
