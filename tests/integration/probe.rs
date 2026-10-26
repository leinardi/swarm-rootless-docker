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

//! The host-side probe's refusals, on the real host.

use crate::harness::*;

/// The host-side probe refuses, before anything is created: a symlink in a path, a parent the
/// consumer could write, a leaf the daemon does not own, and a range the host already hands out.
#[test]
fn the_probe_refuses_unsafe_host_paths_and_ranges() {
    let Some(session) = begin("the_probe_refuses_unsafe_host_paths_and_ranges") else {
        return;
    };
    let mut case = Case::new("probe", 5);
    let root = root();

    let link = format!("{root}/probe-link");
    as_host_root(
        r#"ln -s "$1" "$2""#,
        &[&in_helper(&format!("{root}/probe-data")), &in_helper(&link)],
    );
    let logs = case.refused(
        "symlink",
        &Flags {
            data_root: link,
            ..case.flags()
        },
    );
    assert!(contains(&logs, "is a symlink"), "{logs}");

    let open_parent = format!("{root}/probe-open");
    Case::mkdir_root_owned(&open_parent, "0777");
    as_host_root(
        r#"install -d -o "$1" -g "$1" -m 0750 "$2""#,
        &[UID_TEXT, &in_helper(&format!("{open_parent}/data"))],
    );
    let logs = case.refused(
        "writable-parent",
        &Flags {
            data_root: format!("{open_parent}/data"),
            ..case.flags()
        },
    );
    assert!(contains(&logs, "group- or other-writable"), "{logs}");

    let foreign_leaf = format!("{root}/probe-rootleaf");
    Case::mkdir_root_owned(&foreign_leaf, "0755");
    let logs = case.refused(
        "foreign-leaf",
        &Flags {
            data_root: foreign_leaf,
            ..case.flags()
        },
    );
    assert!(contains(&logs, "not the daemon's UID 568"), "{logs}");

    let host_range = std::fs::read_to_string("/etc/subuid")
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.split(':').nth(1)?.parse::<u32>().ok())
                .find(|start| *start >= 100_000)
        });
    match host_range {
        Some(start) => {
            let logs = case.refused(
                "host-range",
                &Flags {
                    subuid: format!("{start}:{RANGE_COUNT}"),
                    ..case.flags()
                },
            );
            assert!(contains(&logs, "/etc/subuid line"), "{logs}");
        }
        None => note("no host /etc/subuid range at or above 100000; range case skipped"),
    }
    drop(case);
    session.finish();
}

/// The image that sets a POSIX ACL: the launcher's image has no `setfacl`, so `acl` is installed
/// into a throwaway Alpine container (this needs the network).
const ACL_IMAGE: &str = "alpine:3.22";

/// A POSIX ACL that grants another user write on a path refuses it, whatever its mode bits say:
/// an ACL on an ancestor is how a consumer could rename a leaf the mode alone protects.
#[test]
fn a_posix_acl_is_refused_whatever_the_mode() {
    let Some(session) = begin("a_posix_acl_is_refused_whatever_the_mode") else {
        return;
    };
    let mut case = Case::new("acl", 16);
    let (parent, _) = root_parts();
    let mount = format!("{parent}:/parent");
    docker_ok(&[
        "run",
        "--rm",
        "--user",
        "0:0",
        "-v",
        &mount,
        "--entrypoint",
        "/bin/sh",
        ACL_IMAGE,
        "-c",
        r#"apk add --no-cache -q acl && setfacl -m u:1000:rwx "$1""#,
        "sh",
        &in_helper(&case.data_root),
    ]);
    let logs = case.refused("launcher", &case.flags());
    assert!(contains(&logs, "carries a POSIX ACL"), "{logs}");
    drop(case);
    session.finish();
}

/// A mount source that is a hard link to another running instance's mount source is the same
/// file: refused as shared storage, though the two paths have nothing in common.
#[test]
fn a_hard_link_to_another_instances_file_is_refused() {
    let Some(session) = begin("a_hard_link_to_another_instances_file_is_refused") else {
        return;
    };
    let root = root();
    let mut first = Case::new("linka", 17);
    let file = format!("{root}/linka-file");
    as_host_root(
        r#"install -o "$1" -g "$1" -m 0640 /dev/null "$2""#,
        &[UID_TEXT, &in_helper(&file)],
    );
    let mut flags = first.flags();
    flags.mounts.push(format!("{file}:/srd-it-file"));
    let launcher = first.start("launcher", &flags);
    Case::wait_healthy(&launcher);

    let mut second = Case::new("linkb", 18);
    let link = format!("{root}/linkb-file");
    as_host_root(r#"ln "$1" "$2""#, &[&in_helper(&file), &in_helper(&link)]);
    let mut aliased = second.flags();
    aliased.mounts.push(format!("{link}:/srd-it-file"));
    let logs = second.refused("launcher", &aliased);
    assert!(
        contains(&logs, "share storage on the host") && contains(&logs, &link),
        "{logs}"
    );
    drop(second);
    drop(first);
    session.finish();
}
