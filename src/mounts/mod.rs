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

//! Where a host path physically lies, from the mount table (`/proc/self/mountinfo`).
//!
//! Walking a path cannot tell what a bind mount hides: a bind of `/srv/work/tree/subdir` onto
//! `/mnt/alias` starts the walk again at `subdir`, and nothing on the way names `/srv/work/tree`.
//! The mount table does: each mount names its filesystem (`major:minor`) and its root inside that
//! filesystem. So a path is placed as a filesystem plus a position in it (the mount's root joined
//! with the rest of the path), and two placements overlap when they share the filesystem and one
//! position contains the other, however each path was reached.
//!
//! A path with any mount below it is refused: a daemon given it would also see that other
//! filesystem, which another path could reach directly.

#[cfg(test)]
pub mod tests;

use serde::{Deserialize, Serialize};

use crate::config::is_within;

/// The suffix the kernel appends to a mount root whose directory was removed.
const DELETED_SUFFIX: &str = "//deleted";

/// A mount table that does not parse, or a path it cannot place.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MountError {
    #[error("mount table line {line}: {reason}")]
    Malformed { line: usize, reason: &'static str },
    #[error("{path}: the mount table has no root mount")]
    NoMount { path: String },
    #[error("{path}: {count} mounts side by side on {mount_point}, none on top of the others")]
    Ambiguous {
        path: String,
        mount_point: String,
        count: usize,
    },
    #[error("{path}: mount {mount_point} shows its root as deleted")]
    Deleted { path: String, mount_point: String },
    #[error("{path}: {mount_point} is mounted inside it; give the inner path on its own instead")]
    MountBelow { path: String, mount_point: String },
}

/// One line of the mount table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub id: u64,
    pub parent: u64,
    pub device: Device,
    /// The directory of the filesystem this mount shows, from the filesystem's own root.
    pub root: String,
    pub mount_point: String,
}

/// A filesystem, by the `major:minor` the mount table gives it. Bind mounts of one filesystem
/// share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub major: u32,
    pub minor: u32,
}

/// Where a path physically lies: a filesystem and a position in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Placement {
    pub device: Device,
    /// The position from the filesystem's root: clean, absolute, `/` for the root itself.
    pub fs_path: String,
}

impl Placement {
    /// A placement read back from elsewhere (a label), with its position checked.
    ///
    /// # Errors
    ///
    /// The position, when it is not a clean absolute path.
    pub fn new(device: Device, fs_path: &str) -> Result<Self, String> {
        if is_clean_absolute(fs_path) {
            Ok(Self {
                device,
                fs_path: fs_path.to_owned(),
            })
        } else {
            Err(format!("{fs_path:?} is not a clean absolute path"))
        }
    }

    /// Whether the two are the same directory tree, or one lies inside the other.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.device == other.device
            && (is_within(&self.fs_path, &other.fs_path)
                || is_within(&other.fs_path, &self.fs_path))
    }
}

/// Parses a mount table in the `/proc/<pid>/mountinfo` format.
///
/// # Errors
///
/// [`MountError::Malformed`] for the first line that does not parse.
pub fn parse(table: &str) -> Result<Vec<Mount>, MountError> {
    table
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .map(|(index, line)| {
            parse_line(line).map_err(|reason| MountError::Malformed {
                line: index + 1,
                reason,
            })
        })
        .collect()
}

/// Places `path` (clean and absolute, as the reader of `mounts` sees it) on the mount that shows
/// it, resolved as the kernel does: from the namespace's root mount, at each prefix of the path,
/// into the mount attached to the current one there, then up the stack of mounts on that same
/// point. A mount hidden by a later mount over one of its ancestors is attached to a mount the
/// walk has left, so it is never chosen, however deep its mount point.
///
/// # Errors
///
/// A [`MountError`] when there is no single root mount, two mounts are attached side by side on
/// one point, the mount's root was deleted, or another mount lies anywhere below `path` (hidden
/// ones included: refusing is the safe side).
pub fn place(mounts: &[Mount], path: &str) -> Result<Placement, MountError> {
    if let Some(below) = mounts
        .iter()
        .find(|mount| mount.mount_point != path && is_within(&mount.mount_point, path))
    {
        return Err(MountError::MountBelow {
            path: path.to_owned(),
            mount_point: below.mount_point.clone(),
        });
    }
    // The root mount is attached to a mount outside this namespace, so its parent is not listed.
    let roots: Vec<&Mount> = mounts
        .iter()
        .filter(|mount| {
            mount.mount_point == "/" && !mounts.iter().any(|other| other.id == mount.parent)
        })
        .collect();
    let [root] = roots.as_slice() else {
        return Err(if roots.is_empty() {
            MountError::NoMount {
                path: path.to_owned(),
            }
        } else {
            ambiguous(path, "/", roots.len())
        });
    };
    let mut top = top_of_stack(mounts, root, path)?;
    let mut prefix = String::new();
    for component in path
        .split('/')
        .skip(1)
        .filter(|component| !component.is_empty())
    {
        prefix.push('/');
        prefix.push_str(component);
        let attached: Vec<&Mount> = mounts
            .iter()
            .filter(|mount| mount.parent == top.id && mount.mount_point == prefix)
            .collect();
        match attached.as_slice() {
            [] => {}
            [mount] => top = top_of_stack(mounts, mount, path)?,
            [_, _, ..] => return Err(ambiguous(path, &prefix, attached.len())),
        }
    }
    if top.root.ends_with(DELETED_SUFFIX) || !is_clean_absolute(&top.root) {
        return Err(MountError::Deleted {
            path: path.to_owned(),
            mount_point: top.mount_point.clone(),
        });
    }
    let rest = if top.mount_point == "/" {
        path
    } else {
        &path[top.mount_point.len()..]
    };
    let fs_path = match (top.root.as_str(), rest) {
        (root, "") => root.to_owned(),
        ("/", rest) => rest.to_owned(),
        (root, rest) => format!("{root}{rest}"),
    };
    Ok(Placement {
        device: top.device,
        fs_path,
    })
}

/// The mount on top of the stack that starts at `bottom`: each next one is attached to the one
/// below it, on the same point.
fn top_of_stack<'table>(
    mounts: &'table [Mount],
    bottom: &'table Mount,
    path: &str,
) -> Result<&'table Mount, MountError> {
    let mut top = bottom;
    // Each step moves to another mount; more steps than mounts would mean a cycle.
    for _ in 0..mounts.len() {
        let above: Vec<&Mount> = mounts
            .iter()
            .filter(|mount| mount.parent == top.id && mount.mount_point == top.mount_point)
            .collect();
        match above.as_slice() {
            [] => return Ok(top),
            [mount] => top = mount,
            [_, _, ..] => return Err(ambiguous(path, &top.mount_point, above.len())),
        }
    }
    Err(ambiguous(path, &top.mount_point, mounts.len()))
}

fn ambiguous(path: &str, mount_point: &str, count: usize) -> MountError {
    MountError::Ambiguous {
        path: path.to_owned(),
        mount_point: mount_point.to_owned(),
        count,
    }
}

fn parse_line(line: &str) -> Result<Mount, &'static str> {
    let mut fields = line.split(' ');
    let mut next = |what| fields.next().ok_or(what);
    let id = next("no mount ID")?
        .parse()
        .map_err(|_| "mount ID not a number")?;
    let parent = next("no parent ID")?
        .parse()
        .map_err(|_| "parent ID not a number")?;
    let (major, minor) = next("no device")?
        .split_once(':')
        .ok_or("device not major:minor")?;
    let device = Device {
        major: major.parse().map_err(|_| "major not a number")?,
        minor: minor.parse().map_err(|_| "minor not a number")?,
    };
    let root = unescape(next("no root")?)?;
    let mount_point = unescape(next("no mount point")?)?;
    // The options, the optional fields and the separator that ends them must follow.
    next("no mount options")?;
    if !fields.any(|field| field == "-") {
        return Err("no separator before the filesystem type");
    }
    if !is_clean_absolute(&mount_point) {
        return Err("mount point not a clean absolute path");
    }
    Ok(Mount {
        id,
        parent,
        device,
        root,
        mount_point,
    })
}

/// Decodes the kernel's octal escapes (`\040` for a space, `\011`, `\012`, `\134`).
fn unescape(field: &str) -> Result<String, &'static str> {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let digits = bytes.get(index + 1..index + 4).ok_or("short escape")?;
            let text = std::str::from_utf8(digits).map_err(|_| "bad escape")?;
            decoded.push(u8::from_str_radix(text, 8).map_err(|_| "bad escape")?);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "path not UTF-8")
}

fn is_clean_absolute(path: &str) -> bool {
    path == "/"
        || (path.starts_with('/')
            && !path.ends_with('/')
            && path[1..]
                .split('/')
                .all(|component| !matches!(component, "" | "." | "..")))
}
