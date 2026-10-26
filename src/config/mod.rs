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

//! The launcher's per-instance configuration: every flag value parsed into a type whose
//! constructor is the only way to make one, and the rules between flags.
//!
//! These checks are lexical. They catch configuration mistakes early, naming the flag; they are
//! **not** the security boundary. Docker resolves a bind source on the host, which the launcher
//! never sees, so a lexically harmless path can still name something else there (a symlink, a
//! second path to the same directory). The host-side probe decides that, before every create.

#[cfg(test)]
mod tests;

use std::fmt;

use crate::spec::PREFIX;

/// The most extra bind mounts one daemon may get.
pub const MAX_EXTRA_MOUNTS: usize = 16;
/// The most network aliases one daemon may get.
pub const MAX_ALIASES: usize = 8;
/// The lowest first ID of a subordinate range: above every local account on the target hosts
/// (568 for apps, 950 for the admin, 3000 and up for people).
pub const MIN_RANGE_START: u32 = 100_000;
/// The shortest subordinate range: inner images expect IDs up to 65534 (`nobody`).
pub const MIN_RANGE_COUNT: u32 = 65_536;
/// The highest ID a range may reach; 4294967295 is `(uid_t) -1`, never a valid ID.
pub const MAX_RANGE_ID: u32 = u32::MAX - 1;
/// The smallest memory limit: dockerd and its first containers do not start below it.
pub const MIN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
/// The smallest pids limit: dockerd, containerd and their shims alone take dozens.
pub const MIN_PIDS_LIMIT: i64 = 256;
/// The kernel's ceiling for `pid_max`.
pub const MAX_PIDS_LIMIT: i64 = 4_194_304;
/// The most CPUs a daemon may be given, in nano-CPUs.
pub const MAX_NANO_CPUS: i64 = 1024 * NANOS_PER_CPU;

const NANOS_PER_CPU: i64 = 1_000_000_000;
const MAX_PATH_LEN: usize = 4096;
const MAX_INSTANCE_LEN: usize = 32;
const MAX_NETWORK_LEN: usize = 64;
/// One DNS label.
const MAX_ALIAS_LEN: usize = 63;
/// Docker's default bridge, which has no container DNS and refuses network-scoped aliases.
const DEFAULT_BRIDGE: &str = "bridge";

/// Host paths that no bind source may be, lie under, or contain: the host's system directories,
/// its runtime state and its Docker data. A source that contains one (`/`, `/var`) would expose
/// it too.
pub const HOST_SYSTEM_PATHS: &[&str] = &[
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/sys",
    "/usr",
    "/var/lib/docker",
    "/var/run",
];

/// Paths inside the daemon container that an extra mount may not be, lie under, or contain: its
/// system directories, the rootless user's home (data root and socket) and the second upstream
/// `VOLUME`.
pub const RESERVED_CONTAINER_PATHS: &[&str] = &[
    "/bin",
    "/dev",
    "/etc",
    "/home/rootless",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/sys",
    "/usr",
    "/var/lib/docker",
    "/var/run",
];

/// Whether the clean absolute path `path` is `ancestor` or lies under it, component-wise.
#[must_use]
pub fn is_within(path: &str, ancestor: &str) -> bool {
    ancestor == "/"
        || path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A flag value that does not parse or breaks a rule of its own.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    #[error(
        "must be 1 to {MAX_INSTANCE_LEN} lowercase letters, digits or '-', starting and ending with a letter or digit"
    )]
    InstanceName,
    #[error("must be START:COUNT, two decimal numbers")]
    RangeSyntax,
    #[error("START must be at least {MIN_RANGE_START}")]
    RangeStart,
    #[error("COUNT must be at least {MIN_RANGE_COUNT}")]
    RangeCount,
    #[error("the range must end at or below {MAX_RANGE_ID}")]
    RangeEnd,
    #[error("must be an absolute path")]
    NotAbsolute,
    #[error("must be a clean path: no empty, '.' or '..' component and no trailing '/'")]
    NotClean,
    #[error("must not contain ':' or control characters")]
    BadCharacter,
    #[error("must be at most {MAX_PATH_LEN} bytes")]
    TooLong,
    #[error("overlaps the host system path {0}")]
    HostSystemPath(&'static str),
    #[error("overlaps the reserved container path {0}")]
    ReservedContainerPath(&'static str),
    #[error("must not be /")]
    Root,
    #[error("must be SOURCE:TARGET or SOURCE:TARGET:ro or SOURCE:TARGET:rw")]
    MountSyntax,
    #[error("source {0}")]
    MountSource(Box<ValueError>),
    #[error("target {0}")]
    MountTarget(Box<ValueError>),
    #[error(
        "must be 1 to {MAX_NETWORK_LEN} letters, digits, '_', '.' or '-', starting with a letter or digit"
    )]
    NetworkName,
    #[error("must be a named network, not host or none")]
    NetworkMode,
    #[error(
        "must be 1 to {MAX_ALIAS_LEN} lowercase letters, digits or '-', starting and ending with a letter or digit"
    )]
    NetworkAlias,
    #[error("must not start with {PREFIX}, the prefix of the launcher's container names")]
    AliasReserved,
    #[error("must be a whole number of bytes with an optional k, m or g suffix")]
    MemorySyntax,
    #[error("must be at least {} MiB", MIN_MEMORY_BYTES / 1024 / 1024)]
    MemoryTooSmall,
    #[error("is too large")]
    MemoryTooLarge,
    #[error("must be a whole number")]
    PidsSyntax,
    #[error("must be between {MIN_PIDS_LIMIT} and {MAX_PIDS_LIMIT}")]
    PidsRange,
    #[error("must be a decimal number of CPUs with at most 9 decimal places")]
    CpusSyntax,
    #[error("must be above 0 and at most {}", MAX_NANO_CPUS / NANOS_PER_CPU)]
    CpusRange,
}

/// The launcher's configuration is inconsistent across flags.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{first_flag} {first} and {second_flag} {second} must not be the same path or nested")]
    PathOverlap {
        first_flag: &'static str,
        first: String,
        second_flag: &'static str,
        second: String,
    },
    #[error("--mount targets {first} and {second} must not be the same path or nested")]
    TargetOverlap { first: String, second: String },
    #[error("--mount may be given at most {MAX_EXTRA_MOUNTS} times, got {0}")]
    TooManyMounts(usize),
    #[error("--alias may be given at most {MAX_ALIASES} times, got {0}")]
    TooManyAliases(usize),
    #[error("--alias {0} is given twice")]
    DuplicateAlias(String),
    #[error(
        "--alias needs a user-defined network: --network {DEFAULT_BRIDGE} has no container DNS"
    )]
    AliasOnDefaultBridge,
}

/// The instance's name: the daemon container is `swarm-rootless-docker-<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceName(String);

impl InstanceName {
    /// Parses a name: 1 to 32 of `[a-z0-9-]`, starting and ending with a letter or digit.
    ///
    /// # Errors
    ///
    /// [`ValueError::InstanceName`] for anything else.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();
        let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        let valid = !bytes.is_empty()
            && bytes.len() <= MAX_INSTANCE_LEN
            && bytes.iter().all(|byte| edge(byte) || *byte == b'-')
            && bytes.first().is_some_and(edge)
            && bytes.last().is_some_and(edge);
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(ValueError::InstanceName)
        }
    }

    /// The name as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A subordinate ID range: inner ID N ≥ 1 maps to host ID `start + N - 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdRange {
    start: u32,
    count: u32,
}

impl IdRange {
    /// Parses `START:COUNT`.
    ///
    /// # Errors
    ///
    /// A [`ValueError`] for bad syntax, a start below [`MIN_RANGE_START`], a count below
    /// [`MIN_RANGE_COUNT`], or an end above [`MAX_RANGE_ID`].
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let (start, count) = value.split_once(':').ok_or(ValueError::RangeSyntax)?;
        let start = parse_decimal_u32(start).ok_or(ValueError::RangeSyntax)?;
        let count = parse_decimal_u32(count).ok_or(ValueError::RangeSyntax)?;
        if start < MIN_RANGE_START {
            return Err(ValueError::RangeStart);
        }
        if count < MIN_RANGE_COUNT {
            return Err(ValueError::RangeCount);
        }
        let last = start.checked_add(count - 1).ok_or(ValueError::RangeEnd)?;
        if last > MAX_RANGE_ID {
            return Err(ValueError::RangeEnd);
        }
        Ok(Self { start, count })
    }

    /// The first host ID.
    #[must_use]
    pub fn start(self) -> u32 {
        self.start
    }

    /// The number of IDs.
    #[must_use]
    pub fn count(self) -> u32 {
        self.count
    }

    /// The last host ID; never overflows, the constructor checked it.
    #[must_use]
    pub fn last(self) -> u32 {
        self.start + (self.count - 1)
    }

    /// Whether `id` is in the range.
    #[must_use]
    pub fn contains(self, id: u32) -> bool {
        (self.start..=self.last()).contains(&id)
    }

    /// Whether the two ranges share at least one ID.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start <= other.last() && other.start <= self.last()
    }
}

impl fmt::Display for IdRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.start, self.count)
    }
}

/// An absolute, clean path, compared component-wise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsolutePath(String);

impl AbsolutePath {
    /// Parses an absolute path with no empty, `.` or `..` component, no trailing `/`, no `:` (the
    /// `--mount` separator) and no control character.
    ///
    /// # Errors
    ///
    /// A [`ValueError`] naming the first rule broken.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        if !value.starts_with('/') {
            return Err(ValueError::NotAbsolute);
        }
        if value.len() > MAX_PATH_LEN {
            return Err(ValueError::TooLong);
        }
        if value
            .chars()
            .any(|character| character == ':' || character.is_control())
        {
            return Err(ValueError::BadCharacter);
        }
        if value != "/"
            && value[1..]
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(ValueError::NotClean);
        }
        Ok(Self(value.to_owned()))
    }

    /// The path as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this path is `other` or lies under it.
    #[must_use]
    pub fn is_within(&self, other: &str) -> bool {
        is_within(&self.0, other)
    }

    /// Whether one of the two paths is the other or lies under it.
    #[must_use]
    pub fn overlaps(&self, other: &str) -> bool {
        self.is_within(other) || Self(other.to_owned()).is_within(&self.0)
    }

    fn overlapping(&self, list: &'static [&'static str]) -> Option<&'static str> {
        list.iter().copied().find(|entry| self.overlaps(entry))
    }
}

impl fmt::Display for AbsolutePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A host path a bind mount may take as its source: absolute, clean, and clear of
/// [`HOST_SYSTEM_PATHS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPath(AbsolutePath);

impl HostPath {
    /// Parses a host path.
    ///
    /// # Errors
    ///
    /// A [`ValueError`] from [`AbsolutePath::parse`], [`ValueError::Root`], or
    /// [`ValueError::HostSystemPath`] naming the system path it overlaps.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let path = AbsolutePath::parse(value)?;
        if path.as_str() == "/" {
            return Err(ValueError::Root);
        }
        if let Some(system) = path.overlapping(HOST_SYSTEM_PATHS) {
            return Err(ValueError::HostSystemPath(system));
        }
        Ok(Self(path))
    }

    /// The path.
    #[must_use]
    pub fn path(&self) -> &AbsolutePath {
        &self.0
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A path inside the daemon container an extra mount may take as its target: absolute, clean,
/// and clear of [`RESERVED_CONTAINER_PATHS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPath(AbsolutePath);

impl ContainerPath {
    /// Parses a container path.
    ///
    /// # Errors
    ///
    /// A [`ValueError`] from [`AbsolutePath::parse`], [`ValueError::Root`], or
    /// [`ValueError::ReservedContainerPath`] naming the reserved path it overlaps.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let path = AbsolutePath::parse(value)?;
        if path.as_str() == "/" {
            return Err(ValueError::Root);
        }
        if let Some(reserved) = path.overlapping(RESERVED_CONTAINER_PATHS) {
            return Err(ValueError::ReservedContainerPath(reserved));
        }
        Ok(Self(path))
    }

    /// The path.
    #[must_use]
    pub fn path(&self) -> &AbsolutePath {
        &self.0
    }
}

impl fmt::Display for ContainerPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One extra bind mount: a host directory or file at a container path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraMount {
    source: HostPath,
    target: ContainerPath,
    read_only: bool,
}

impl ExtraMount {
    /// Parses `SOURCE:TARGET`, `SOURCE:TARGET:ro` or `SOURCE:TARGET:rw`.
    ///
    /// # Errors
    ///
    /// [`ValueError::MountSyntax`], or the source's or target's own error, wrapped to say which.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let mut parts = value.split(':');
        let (Some(source), Some(target)) = (parts.next(), parts.next()) else {
            return Err(ValueError::MountSyntax);
        };
        let read_only = match parts.next() {
            None | Some("rw") => false,
            Some("ro") => true,
            Some(_) => return Err(ValueError::MountSyntax),
        };
        if parts.next().is_some() {
            return Err(ValueError::MountSyntax);
        }
        let source =
            HostPath::parse(source).map_err(|error| ValueError::MountSource(Box::new(error)))?;
        let target = ContainerPath::parse(target)
            .map_err(|error| ValueError::MountTarget(Box::new(error)))?;
        Ok(Self {
            source,
            target,
            read_only,
        })
    }

    /// The host source.
    #[must_use]
    pub fn source(&self) -> &HostPath {
        &self.source
    }

    /// The container target.
    #[must_use]
    pub fn target(&self) -> &ContainerPath {
        &self.target
    }

    /// Whether the mount is read-only.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.read_only
    }
}

impl fmt::Display for ExtraMount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = if self.read_only { "ro" } else { "rw" };
        write!(formatter, "{}:{}:{mode}", self.source, self.target)
    }
}

/// The Docker network the daemon container joins: a named network, never `host` or `none`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkName(String);

impl NetworkName {
    /// Parses a network name. The character set excludes `:`, so `container:<id>` cannot pass.
    ///
    /// # Errors
    ///
    /// [`ValueError::NetworkName`] for a malformed name, [`ValueError::NetworkMode`] for `host`
    /// or `none` in any case.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= MAX_NETWORK_LEN
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !valid {
            return Err(ValueError::NetworkName);
        }
        if value.eq_ignore_ascii_case("host") || value.eq_ignore_ascii_case("none") {
            return Err(ValueError::NetworkMode);
        }
        Ok(Self(value.to_owned()))
    }

    /// The name as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NetworkName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A name the daemon container also answers to on its network: one DNS label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAlias(String);

impl NetworkAlias {
    /// Parses an alias: 1 to 63 of `[a-z0-9-]`, starting and ending with a letter or digit, and
    /// not starting with [`PREFIX`], so it never names one of the launcher's containers.
    ///
    /// # Errors
    ///
    /// [`ValueError::NetworkAlias`] for a malformed alias, [`ValueError::AliasReserved`] for one
    /// with the launcher's prefix.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let bytes = value.as_bytes();
        let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        let valid = !bytes.is_empty()
            && bytes.len() <= MAX_ALIAS_LEN
            && bytes.iter().all(|byte| edge(byte) || *byte == b'-')
            && bytes.first().is_some_and(edge)
            && bytes.last().is_some_and(edge);
        if !valid {
            return Err(ValueError::NetworkAlias);
        }
        if value.starts_with(PREFIX) {
            return Err(ValueError::AliasReserved);
        }
        Ok(Self(value.to_owned()))
    }

    /// The alias as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NetworkAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The daemon container's memory limit, in bytes; swap is set to the same value, so none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBytes(i64);

impl MemoryBytes {
    /// Parses a whole number of bytes with an optional `k`, `m` or `g` suffix (powers of 1024,
    /// either case), as `docker run --memory` does.
    ///
    /// # Errors
    ///
    /// A [`ValueError`] for bad syntax, a value below [`MIN_MEMORY_BYTES`], or one that does not
    /// fit the API's signed 64-bit field.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let (digits, multiplier) = match value.as_bytes().last() {
            Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
            Some(b'm' | b'M') => (&value[..value.len() - 1], 1024 * 1024),
            Some(b'g' | b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
            _ => (value, 1),
        };
        let number = parse_decimal_u64(digits).ok_or(ValueError::MemorySyntax)?;
        let bytes = number
            .checked_mul(multiplier)
            .ok_or(ValueError::MemoryTooLarge)?;
        if bytes < MIN_MEMORY_BYTES {
            return Err(ValueError::MemoryTooSmall);
        }
        i64::try_from(bytes)
            .map(Self)
            .map_err(|_| ValueError::MemoryTooLarge)
    }

    /// The limit in bytes, as the API takes it.
    #[must_use]
    pub fn bytes(self) -> i64 {
        self.0
    }
}

/// The daemon container's pids limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidsLimit(i64);

impl PidsLimit {
    /// Parses a whole number between [`MIN_PIDS_LIMIT`] and [`MAX_PIDS_LIMIT`].
    ///
    /// # Errors
    ///
    /// [`ValueError::PidsSyntax`] or [`ValueError::PidsRange`].
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let number = parse_decimal_u64(value).ok_or(ValueError::PidsSyntax)?;
        let limit = i64::try_from(number).map_err(|_| ValueError::PidsRange)?;
        if (MIN_PIDS_LIMIT..=MAX_PIDS_LIMIT).contains(&limit) {
            Ok(Self(limit))
        } else {
            Err(ValueError::PidsRange)
        }
    }

    /// The limit, as the API takes it.
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

/// The daemon container's CPU limit, in nano-CPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NanoCpus(i64);

impl NanoCpus {
    /// Parses a decimal number of CPUs (`2`, `1.5`, `0.25`) exactly, without floating point.
    ///
    /// # Errors
    ///
    /// [`ValueError::CpusSyntax`] or [`ValueError::CpusRange`].
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if value.ends_with('.') || fraction.len() > 9 {
            return Err(ValueError::CpusSyntax);
        }
        let whole = parse_decimal_u64(whole).ok_or(ValueError::CpusSyntax)?;
        let fraction_nanos = if fraction.is_empty() {
            0
        } else {
            let digits = parse_decimal_u64(fraction).ok_or(ValueError::CpusSyntax)?;
            // The digits are at most nine, so the scale is 10^(9 - len) and the product fits.
            let scale =
                10_u64.pow(9 - u32::try_from(fraction.len()).map_err(|_| ValueError::CpusSyntax)?);
            digits * scale
        };
        let nanos = whole
            .checked_mul(1_000_000_000)
            .and_then(|nanos| nanos.checked_add(fraction_nanos))
            .and_then(|nanos| i64::try_from(nanos).ok())
            .ok_or(ValueError::CpusRange)?;
        if nanos > 0 && nanos <= MAX_NANO_CPUS {
            Ok(Self(nanos))
        } else {
            Err(ValueError::CpusRange)
        }
    }

    /// The limit in nano-CPUs, as the API takes it.
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

/// Everything one launcher needs to know about its daemon, validated as a whole.
///
/// The fields are private: [`LaunchConfig::new`] is the only way to make one, so a value that
/// broke a rule between flags cannot be assigned afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    instance: InstanceName,
    subuid: IdRange,
    subgid: IdRange,
    data_root: HostPath,
    socket_dir: HostPath,
    mounts: Vec<ExtraMount>,
    network: NetworkName,
    aliases: Vec<NetworkAlias>,
    memory: MemoryBytes,
    pids_limit: PidsLimit,
    cpus: Option<NanoCpus>,
}

/// The parsed but not yet cross-checked flag values; [`LaunchConfig::new`] checks them.
#[derive(Debug, Clone)]
pub struct LaunchValues {
    pub instance: InstanceName,
    pub subuid: IdRange,
    pub subgid: Option<IdRange>,
    pub data_root: HostPath,
    pub socket_dir: HostPath,
    pub mounts: Vec<ExtraMount>,
    pub network: NetworkName,
    pub aliases: Vec<NetworkAlias>,
    pub memory: MemoryBytes,
    pub pids_limit: PidsLimit,
    pub cpus: Option<NanoCpus>,
}

impl LaunchConfig {
    /// Checks the rules between flags: at most [`MAX_EXTRA_MOUNTS`] mounts; the data root, the
    /// socket directory and every mount source pairwise distinct and not nested; mount targets
    /// pairwise distinct and not nested; at most [`MAX_ALIASES`] aliases, pairwise distinct, and
    /// none on the default bridge. `--subgid` defaults to `--subuid`.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] found.
    pub fn new(values: LaunchValues) -> Result<Self, ConfigError> {
        if values.mounts.len() > MAX_EXTRA_MOUNTS {
            return Err(ConfigError::TooManyMounts(values.mounts.len()));
        }
        let mut sources: Vec<(&'static str, &AbsolutePath)> = vec![
            ("--data-root", values.data_root.path()),
            ("--socket-dir", values.socket_dir.path()),
        ];
        sources.extend(
            values
                .mounts
                .iter()
                .map(|mount| ("--mount source", mount.source().path())),
        );
        for (index, (first_flag, first)) in sources.iter().enumerate() {
            for (second_flag, second) in &sources[index + 1..] {
                if first.overlaps(second.as_str()) {
                    return Err(ConfigError::PathOverlap {
                        first_flag,
                        first: first.to_string(),
                        second_flag,
                        second: second.to_string(),
                    });
                }
            }
        }
        for (index, first) in values.mounts.iter().enumerate() {
            for second in &values.mounts[index + 1..] {
                if first
                    .target()
                    .path()
                    .overlaps(second.target().path().as_str())
                {
                    return Err(ConfigError::TargetOverlap {
                        first: first.target().to_string(),
                        second: second.target().to_string(),
                    });
                }
            }
        }
        if values.aliases.len() > MAX_ALIASES {
            return Err(ConfigError::TooManyAliases(values.aliases.len()));
        }
        for (index, alias) in values.aliases.iter().enumerate() {
            if values.aliases[..index].contains(alias) {
                return Err(ConfigError::DuplicateAlias(alias.to_string()));
            }
        }
        if !values.aliases.is_empty() && values.network.as_str() == DEFAULT_BRIDGE {
            return Err(ConfigError::AliasOnDefaultBridge);
        }
        Ok(Self {
            instance: values.instance,
            subuid: values.subuid,
            subgid: values.subgid.unwrap_or(values.subuid),
            data_root: values.data_root,
            socket_dir: values.socket_dir,
            mounts: values.mounts,
            network: values.network,
            aliases: values.aliases,
            memory: values.memory,
            pids_limit: values.pids_limit,
            cpus: values.cpus,
        })
    }

    /// The instance name.
    #[must_use]
    pub fn instance(&self) -> &InstanceName {
        &self.instance
    }

    /// The subordinate UID range.
    #[must_use]
    pub fn subuid(&self) -> IdRange {
        self.subuid
    }

    /// The subordinate GID range.
    #[must_use]
    pub fn subgid(&self) -> IdRange {
        self.subgid
    }

    /// The host directory of the data root.
    #[must_use]
    pub fn data_root(&self) -> &HostPath {
        &self.data_root
    }

    /// The host directory of the socket.
    #[must_use]
    pub fn socket_dir(&self) -> &HostPath {
        &self.socket_dir
    }

    /// The extra bind mounts.
    #[must_use]
    pub fn mounts(&self) -> &[ExtraMount] {
        &self.mounts
    }

    /// The network the daemon container joins.
    #[must_use]
    pub fn network(&self) -> &NetworkName {
        &self.network
    }

    /// The daemon container's aliases on its network.
    #[must_use]
    pub fn aliases(&self) -> &[NetworkAlias] {
        &self.aliases
    }

    /// The memory limit.
    #[must_use]
    pub fn memory(&self) -> MemoryBytes {
        self.memory
    }

    /// The pids limit.
    #[must_use]
    pub fn pids_limit(&self) -> PidsLimit {
        self.pids_limit
    }

    /// The CPU limit, if any.
    #[must_use]
    pub fn cpus(&self) -> Option<NanoCpus> {
        self.cpus
    }
}

/// ASCII digits only: no sign, no space, no underscore, no other base.
fn parse_decimal_u64(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn parse_decimal_u32(value: &str) -> Option<u32> {
    parse_decimal_u64(value).and_then(|number| u32::try_from(number).ok())
}
