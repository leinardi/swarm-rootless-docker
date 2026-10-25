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

//! The host-side probe: what the `probe` subcommand checks, in a container that sees the host's
//! `/` read-only at [`PROBE_HOST_ROOT`], before the launcher creates a daemon. This, not the
//! lexical flag rules, is the security boundary for the host paths and ranges.
//!
//! For each host path the daemon will get (data root, socket directory, mount sources):
//!
//! - no component is a symlink, so the path names what it says;
//! - every ancestor is a directory owned by root, not group- or other-writable, and every
//!   component, the path itself included, carries no POSIX ACL and only a trivial NFSv4 ACL
//!   ([`nfs4`]): nobody but root can rename or replace a component, so the path cannot be
//!   switched between the probe and the create, or before a restart;
//! - the path itself has the expected type, and the data root and socket directory belong to the
//!   daemon's UID and are not group- or other-writable;
//! - its physical location ([`crate::mounts`]) overlaps none of the request's other paths and
//!   nothing a denied path holds (the host's system paths, its Docker data root and socket): the
//!   denied path as written and with its symlinks followed, and every filesystem mounted at or
//!   below it, so no bind mount elsewhere reaches them; and no mount lies below it.
//!
//! And for the ranges ([`ids`]): no account or group of the host is inside them, and no range of
//! the host's subordinate ID files overlaps them.

mod host;
pub mod ids;
pub mod nfs4;
#[cfg(test)]
mod tests;

use std::io;

use serde::{Deserialize, Serialize};

pub use host::RealHost;

use crate::config::{AbsolutePath, HOST_SYSTEM_PATHS, HostPath, is_within};
use crate::mounts::{self, Mount, Placement};
use crate::spec::{FileId, HostLocation, PROBE_HOST_ROOT, ProbeRequest, ValidProbeRequest};

/// Where the probe reads its own mount table.
pub const MOUNTINFO: &str = "/proc/self/mountinfo";
/// The POSIX ACL xattrs; either one present refuses.
pub const POSIX_ACL_XATTRS: [&str; 2] = ["system.posix_acl_access", "system.posix_acl_default"];

const TYPE_MASK: u32 = 0o170_000;
/// The kernel's own limit on symlinks followed in one lookup.
const MAX_SYMLINK_HOPS: usize = 40;
const GROUP_OTHER_WRITE: u32 = 0o022;

/// A file's metadata, as `lstat` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub uid: u32,
    /// The permission bits, `0o7777`.
    pub mode: u32,
    pub device: u64,
    pub inode: u64,
}

/// A file's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
    Symlink,
    Other,
}

/// What the probe reads from the host. [`RealHost`] does it with system calls; the tests use a
/// map.
pub trait Host {
    /// `lstat`.
    ///
    /// # Errors
    ///
    /// The system call's error, not found included.
    fn lstat(&self, path: &str) -> io::Result<Meta>;

    /// An xattr of `path` itself (never of a symlink's target): `None` when it is absent or the
    /// filesystem does not support it.
    ///
    /// # Errors
    ///
    /// Any other error, a value too large to read included.
    fn xattr(&self, path: &str, name: &str) -> io::Result<Option<Vec<u8>>>;

    /// A symlink's target, as stored.
    ///
    /// # Errors
    ///
    /// The system call's error, or a target that is not UTF-8.
    fn readlink(&self, path: &str) -> io::Result<String>;

    /// A whole file: `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Any other error.
    fn read(&self, path: &str) -> io::Result<Option<String>>;
}

/// What the probe prints: one JSON line on stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProbeReport {
    /// Every check passed: the location of each path, in the request's order.
    Accepted {
        data_root: HostLocation,
        socket_dir: HostLocation,
        mounts: Vec<HostLocation>,
    },
    /// A check failed, or the probe could not tell.
    Refused { reason: String },
}

/// What the type of the path itself must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    /// The data root and the socket directory: a directory owned by the daemon's UID.
    Owned,
    /// A mount source: a directory or a regular file.
    Source,
}

/// Runs the probe for a request as it arrived (the JSON argument). Never fails: whatever goes
/// wrong is a refusal.
#[must_use]
pub fn run(host: &impl Host, root: &str, request: &str) -> ProbeReport {
    let checked = serde_json::from_str::<ProbeRequest>(request)
        .map_err(|error| format!("the request does not parse: {error}"))
        .and_then(|request| request.validate());
    match checked.and_then(|request| probe(host, root, &request)) {
        Ok(report) => report,
        Err(reason) => ProbeReport::Refused { reason },
    }
}

/// [`run`] on the real host, at [`PROBE_HOST_ROOT`].
#[must_use]
pub fn run_on_host(request: &str) -> ProbeReport {
    run(&RealHost, PROBE_HOST_ROOT, request)
}

fn probe(host: &impl Host, root: &str, request: &ValidProbeRequest) -> Result<ProbeReport, String> {
    let table = host
        .read(MOUNTINFO)
        .map_err(|error| format!("read {MOUNTINFO}: {error}"))?
        .ok_or_else(|| format!("{MOUNTINFO} does not exist"))?;
    let table = mounts::parse(&table).map_err(|error| error.to_string())?;
    let mut denied = Vec::new();
    for path in HOST_SYSTEM_PATHS
        .iter()
        .copied()
        .chain(request.denied.iter().map(AbsolutePath::as_str))
    {
        for placement in denied_storage(host, root, &table, path)
            .map_err(|reason| format!("denied path {path}: {reason}"))?
        {
            denied.push((path, placement));
        }
    }

    let mut paths = vec![
        ("--data-root", &request.data_root, Leaf::Owned),
        ("--socket-dir", &request.socket_dir, Leaf::Owned),
    ];
    paths.extend(
        request
            .mount_sources
            .iter()
            .map(|source| ("--mount source", source, Leaf::Source)),
    );
    let mut located: Vec<(&str, &HostPath, HostLocation)> = Vec::new();
    for (flag, path, leaf) in paths {
        let location = check_path(host, root, &table, path, leaf, request.owner_uid)
            .map_err(|reason| format!("{flag} {path}: {reason}"))?;
        for (denied_path, placement) in &denied {
            if path.path().overlaps(denied_path) || location.placement.overlaps(placement) {
                return Err(format!(
                    "{flag} {path} reaches the storage of {denied_path}, which no daemon may get"
                ));
            }
        }
        for (other_flag, other, other_location) in &located {
            if location.overlaps(other_location) {
                return Err(format!(
                    "{flag} {path} and {other_flag} {other} share storage on the host"
                ));
            }
        }
        located.push((flag, path, location));
    }
    ids::check(host, root, request.subuid, request.subgid)?;

    let mut locations = located.into_iter().map(|(_, _, location)| location);
    let (Some(data_root), Some(socket_dir)) = (locations.next(), locations.next()) else {
        return Err("internal: the data root and socket directory were not located".to_owned());
    };
    Ok(ProbeReport::Accepted {
        data_root,
        socket_dir,
        mounts: locations.collect(),
    })
}

/// Everything a denied path holds, physically: where it lies, both as written and with every
/// symlink on it followed, and the whole filesystem tree of every mount at or below it (hidden
/// ones included). A daemon path overlapping any of these reaches its storage, however it was
/// bound.
fn denied_storage(
    host: &impl Host,
    root: &str,
    table: &[Mount],
    path: &str,
) -> Result<Vec<Placement>, String> {
    let resolved = resolve(host, root, path)?;
    let mut placements = Vec::new();
    for shown in [path, resolved.as_str()] {
        let full = on_host(root, shown);
        placements.push(mounts::locate(table, &full).map_err(|error| error.to_string())?);
        placements.extend(
            table
                .iter()
                .filter(|mount| is_within(&mount.mount_point, &full))
                .map(|mount| {
                    // A root that cannot be placed (deleted) denies its whole filesystem.
                    Placement::new(mount.device, &mount.root).unwrap_or(Placement {
                        device: mount.device,
                        fs_path: "/".to_owned(),
                    })
                }),
        );
    }
    Ok(placements)
}

/// `path` with every symlink on it followed inside `root`, where an absolute target starts again
/// at the host's `/`. A component that does not exist is kept as written, and so is the rest.
fn resolve(host: &impl Host, root: &str, path: &str) -> Result<String, String> {
    let mut pending: Vec<String> = path.split('/').rev().map(str::to_owned).collect();
    let mut resolved: Vec<String> = Vec::new();
    let mut hops = 0;
    while let Some(component) = pending.pop() {
        match component.as_str() {
            "" | "." => continue,
            ".." => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let candidate = format!(
            "/{}",
            [resolved.as_slice(), std::slice::from_ref(&component)]
                .concat()
                .join("/")
        );
        let full = on_host(root, &candidate);
        match host.lstat(&full) {
            Ok(meta) if meta.kind == Kind::Symlink => {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return Err(format!("more than {MAX_SYMLINK_HOPS} symlinks"));
                }
                let target = host
                    .readlink(&full)
                    .map_err(|error| format!("readlink {candidate}: {error}"))?;
                if target.starts_with('/') {
                    resolved.clear();
                }
                pending.extend(target.split('/').rev().map(str::to_owned));
            }
            Ok(_) => resolved.push(component),
            Err(error) if error.kind() == io::ErrorKind::NotFound => resolved.push(component),
            Err(error) => return Err(format!("lstat {candidate}: {error}")),
        }
    }
    Ok(format!("/{}", resolved.join("/")))
}

/// Walks `path` component by component, as the probe sees it under `root`.
fn check_path(
    host: &impl Host,
    root: &str,
    table: &[Mount],
    path: &HostPath,
    leaf: Leaf,
    owner_uid: u32,
) -> Result<HostLocation, String> {
    let mut prefix = String::new();
    let mut components: Vec<&str> = vec![""];
    components.extend(path.path().as_str().split('/').skip(1));
    let last = components.len() - 1;
    let mut leaf_meta = None;
    for (index, component) in components.into_iter().enumerate() {
        if index > 0 {
            prefix.push('/');
            prefix.push_str(component);
        }
        let shown = if prefix.is_empty() { "/" } else { &prefix };
        let full = on_host(root, shown);
        let meta = host.lstat(&full).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!("{shown} does not exist")
            } else {
                format!("lstat {shown}: {error}")
            }
        })?;
        if meta.kind == Kind::Symlink {
            return Err(format!("{shown} is a symlink"));
        }
        check_acls(host, &full, meta.mode).map_err(|reason| format!("{shown}: {reason}"))?;
        if index < last {
            if meta.kind != Kind::Directory {
                return Err(format!("{shown} is not a directory"));
            }
            if meta.uid != 0 {
                return Err(format!(
                    "{shown} belongs to UID {}; every parent must belong to root",
                    meta.uid
                ));
            }
            if meta.mode & GROUP_OTHER_WRITE != 0 {
                return Err(format!(
                    "{shown} is group- or other-writable (mode {:o}); every parent must not be",
                    meta.mode
                ));
            }
        } else {
            leaf_meta = Some(meta);
        }
    }
    let meta = leaf_meta.ok_or_else(|| "internal: no components".to_owned())?;
    match (leaf, meta.kind) {
        (Leaf::Owned, Kind::Directory) => {
            if meta.uid != owner_uid {
                return Err(format!(
                    "belongs to UID {}, not the daemon's UID {owner_uid}",
                    meta.uid
                ));
            }
            if meta.mode & GROUP_OTHER_WRITE != 0 {
                return Err(format!(
                    "is group- or other-writable (mode {:o})",
                    meta.mode
                ));
            }
        }
        (Leaf::Source, Kind::Directory | Kind::File) => {}
        (Leaf::Owned, Kind::File | Kind::Symlink | Kind::Other) => {
            return Err("is not a directory".to_owned());
        }
        (Leaf::Source, Kind::Symlink | Kind::Other) => {
            return Err("is neither a directory nor a regular file".to_owned());
        }
    }
    let placement = mounts::place(table, &on_host(root, path.path().as_str()))
        .map_err(|error| error.to_string())?;
    Ok(HostLocation {
        placement,
        file: FileId {
            device: meta.device,
            inode: meta.inode,
        },
    })
}

/// No POSIX ACL, and an NFSv4 ACL only when trivial for `mode`.
fn check_acls(host: &impl Host, full: &str, mode: u32) -> Result<(), String> {
    for name in POSIX_ACL_XATTRS {
        if host
            .xattr(full, name)
            .map_err(|error| format!("read {name}: {error}"))?
            .is_some()
        {
            return Err(format!("carries a POSIX ACL ({name})"));
        }
    }
    if let Some(bytes) = host
        .xattr(full, nfs4::XATTR)
        .map_err(|error| format!("read {}: {error}", nfs4::XATTR))?
    {
        let acl = nfs4::decode(&bytes).map_err(|error| error.to_string())?;
        nfs4::check_trivial(&acl, mode).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The path the probe uses for host path `path`.
fn on_host(root: &str, path: &str) -> String {
    match (root, path) {
        ("/", path) => path.to_owned(),
        (root, "/") => root.to_owned(),
        (root, path) => format!("{root}{path}"),
    }
}

/// The permission bits of a raw `st_mode`, and its type.
fn split_mode(raw: u32) -> (Kind, u32) {
    let kind = match raw & TYPE_MASK {
        0o040_000 => Kind::Directory,
        0o100_000 => Kind::File,
        0o120_000 => Kind::Symlink,
        _ => Kind::Other,
    };
    (kind, raw & 0o7777)
}
