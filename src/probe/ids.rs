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

//! The ranges against the host's own IDs: no account (`/etc/passwd`, its primary GID too) or
//! group (`/etc/group`) of the host inside them, and no range of `/etc/subuid` or `/etc/subgid`
//! overlapping them. A line that does not parse refuses: the probe cannot tell what it holds.
//!
//! Accounts from a directory service (LDAP, Active Directory) are not in these files and not
//! checked; that range allocation is the host admin's, as the README says.

use crate::config::IdRange;

use super::Host;

/// Checks both ranges against the host files under `root`.
///
/// # Errors
///
/// A message naming the account, group or range inside or overlapping, or the file and line
/// that does not parse.
pub fn check(host: &impl Host, root: &str, uids: IdRange, gids: IdRange) -> Result<(), String> {
    let read = |file: &str, required: bool| -> Result<String, String> {
        let path = super::on_host(root, file);
        match host.read(&path) {
            Ok(Some(text)) => Ok(text),
            Ok(None) if !required => Ok(String::new()),
            Ok(None) => Err(format!("{file} does not exist")),
            Err(error) => Err(format!("read {file}: {error}")),
        }
    };
    for (line, (name, uid, gid)) in passwd(&read("/etc/passwd", true)?)? {
        if uids.contains(uid) {
            return Err(format!(
                "--subuid {uids} contains UID {uid} of host account {name} (/etc/passwd line {line})"
            ));
        }
        if gids.contains(gid) {
            return Err(format!(
                "--subgid {gids} contains GID {gid}, primary group of host account {name} (/etc/passwd line {line})"
            ));
        }
    }
    for (line, (name, gid)) in group(&read("/etc/group", true)?)? {
        if gids.contains(gid) {
            return Err(format!(
                "--subgid {gids} contains GID {gid} of host group {name} (/etc/group line {line})"
            ));
        }
    }
    for (file, flag, range) in [
        ("/etc/subuid", "--subuid", uids),
        ("/etc/subgid", "--subgid", gids),
    ] {
        for (line, (name, start, count)) in subids(&read(file, false)?, file)? {
            let last = start + count - 1;
            if u64::from(range.start()) <= last && start <= u64::from(range.last()) {
                return Err(format!(
                    "{flag} {range} overlaps {name}:{start}:{count} ({file} line {line})"
                ));
            }
        }
    }
    Ok(())
}

type Numbered<T> = Vec<(usize, T)>;

/// `name:password:uid:gid:…` lines.
fn passwd(text: &str) -> Result<Numbered<(&str, u32, u32)>, String> {
    lines(text, "/etc/passwd", |fields| {
        let [name, _, uid, gid, ..] = fields else {
            return None;
        };
        Some((*name, id(uid)?, id(gid)?))
    })
}

/// `name:password:gid:members` lines.
fn group(text: &str) -> Result<Numbered<(&str, u32)>, String> {
    lines(text, "/etc/group", |fields| {
        let [name, _, gid, ..] = fields else {
            return None;
        };
        Some((*name, id(gid)?))
    })
}

/// `name:start:count` lines, a count of at least one.
fn subids<'text>(text: &'text str, file: &str) -> Result<Numbered<(&'text str, u64, u64)>, String> {
    lines(text, file, |fields| {
        let [name, start, count] = fields else {
            return None;
        };
        let count = u64::from(id(count)?);
        (count > 0).then_some((*name, u64::from(id(start)?), count))
    })
}

fn lines<'text, T>(
    text: &'text str,
    file: &str,
    parse: impl Fn(&[&'text str]) -> Option<T>,
) -> Result<Numbered<T>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let fields: Vec<&str> = line.split(':').collect();
            parse(&fields)
                .map(|entry| (index + 1, entry))
                .ok_or_else(|| format!("{file} line {} does not parse", index + 1))
        })
        .collect()
}

fn id(field: &str) -> Option<u32> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}
