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

//! A daemon's life: its profile and engine, ownership in the workspace, stop, kill, and a foreign
//! container with its name.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::*;

/// Whether a `/proc/net/tcp{,6}` local address (hex, in the kernel's byte order) is loopback:
/// 127.0.0.0/8, or `::1`.
fn is_loopback(address: &str) -> bool {
    match address.len() {
        8 => address.ends_with("7F"),
        32 => address == "00000000000000000000000001000000",
        _ => false,
    }
}

/// The daemon is the fixed profile, from the launcher's own image, every process as the daemon
/// UID.
fn check_profile(launcher: &str, daemon: &str) {
    let spec = inspect(daemon).unwrap_or_else(|| panic!("no daemon {daemon}"));
    let me = inspect(launcher).unwrap_or_else(|| panic!("no launcher"));
    assert_eq!(spec["Image"], me["Image"], "the daemon runs another image");
    assert_eq!(spec["Config"]["User"], "568:568");
    let host = &spec["HostConfig"];
    assert_eq!(host["Privileged"], false);
    assert_eq!(host["CapDrop"], serde_json::json!(["ALL"]));
    assert_eq!(host["CapAdd"], serde_json::json!(["SETUID", "SETGID"]));
    assert!(
        host["PortBindings"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "{}",
        host["PortBindings"]
    );
    assert_eq!(host["NetworkMode"], NETWORK);

    let uids = docker_ok(&["top", daemon, "-eo", "pid,uid"]);
    let uids: BTreeSet<&str> = uids
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    assert_eq!(
        uids,
        BTreeSet::from(["568"]),
        "daemon processes run as {uids:?}"
    );
}

/// The consumer sees an empty rootless engine, outside any swarm, and nothing listens on TCP.
fn check_engine(case: &Case, daemon: &str) {
    let info: Value = must(
        serde_json::from_str(&case.consumer(&["info", "--format", "{{json .}}"])),
        "parse docker info",
    );
    let security = info["SecurityOptions"].to_string();
    assert!(contains(&security, "name=rootless"), "{security}");
    assert_eq!(info["Containers"], 0);
    assert_eq!(info["Swarm"]["LocalNodeState"], "inactive");
    note(&format!("inner storage driver: {}", info["Driver"]));

    let tables = docker_ok(&[
        "exec",
        daemon,
        "sh",
        "-c",
        "cat /proc/[0-9]*/net/tcp /proc/[0-9]*/net/tcp6 2>/dev/null",
    ]);
    // Docker's embedded DNS listens on 127.0.0.11 in every container of a user-defined network:
    // loopback is fine. A listener on any other address, or the Docker API ports anywhere, is not.
    let listening: Vec<&str> = tables
        .lines()
        .filter(|line| line.split_whitespace().nth(3) == Some("0A"))
        .filter(|line| {
            let local = line.split_whitespace().nth(1).unwrap_or_default();
            let (address, port) = local.split_once(':').unwrap_or_default();
            let api_port = matches!(port, "0947" | "0948");
            api_port || !is_loopback(address)
        })
        .collect();
    assert!(listening.is_empty(), "TCP listeners: {listening:?}");
}

/// Files the consumer's containers write to the workspace belong to the consumer (inner root) or
/// to the subordinate range (any other inner UID).
fn check_workspace(case: &Case) {
    case.load_inner_image();
    let work = case.work.clone();
    let bind = format!("{work}:{work}");
    case.consumer(&[
        "run",
        "--rm",
        "--network",
        "none",
        "-v",
        &bind,
        INNER_IMAGE,
        "sh",
        "-c",
        // The workspace leaf is 0750: a non-root inner UID (a subordinate host UID) cannot
        // even traverse it until the consumer opens it, as the README tells non-root images to.
        &format!("touch {work}/by-root && chmod 0711 {work} && mkdir -m 0777 {work}/open"),
    ]);
    case.consumer(&[
        "run",
        "--rm",
        "--network",
        "none",
        "--user",
        "1000:1000",
        "-v",
        &bind,
        INNER_IMAGE,
        "touch",
        &format!("{work}/open/by-1000"),
    ]);
    let owners = as_host_root(
        r#"stat -c %u:%g "$@""#,
        &[
            &in_helper(&format!("{work}/by-root")),
            &in_helper(&format!("{work}/open/by-1000")),
        ],
    );
    let mapped = case.range_start + 999;
    assert_eq!(owners, format!("568:568\n{mapped}:{mapped}"));
}

/// The whole life of one daemon: the launcher becomes healthy; the daemon is the fixed profile,
/// from the launcher's own image, running as the daemon UID; the consumer sees a rootless engine
/// with nothing in it; files its containers write belong to the consumer (inner root) or to the
/// subordinate range; nothing listens on TCP; SIGTERM removes the daemon in time; and no anonymous
/// volume is left behind.
#[test]
fn a_daemon_runs_serves_the_consumer_and_stops() {
    let Some(session) = begin("a_daemon_runs_serves_the_consumer_and_stops") else {
        return;
    };
    let mut case = Case::new("life", 0);
    let flags = case.flags();
    let launcher = case.start("launcher", &flags);
    Case::wait_healthy(&launcher);
    let daemon = Case::daemon(&flags);

    check_profile(&launcher, &daemon);
    check_engine(&case, &daemon);
    check_workspace(&case);

    let stopping = Instant::now();
    docker_ok(&["stop", "-t", "40", &launcher]);
    let (code, logs) = Case::wait_exit(&launcher, Duration::from_secs(5));
    assert!(
        stopping.elapsed() < Duration::from_secs(40),
        "{:?}",
        stopping.elapsed()
    );
    assert_eq!(code, 0, "the launcher exited {code}:\n{logs}");
    assert!(!exists(&daemon), "the daemon outlived its launcher");
    drop(case);
    session.finish();
}

/// A launcher killed with SIGKILL leaves its daemon; the next launcher of the instance removes the
/// orphan and starts its own.
#[test]
fn a_killed_launchers_daemon_is_replaced() {
    let Some(session) = begin("a_killed_launchers_daemon_is_replaced") else {
        return;
    };
    let mut case = Case::new("orphan", 1);
    let flags = case.flags();
    let first = case.start("first", &flags);
    Case::wait_healthy(&first);
    let daemon = Case::daemon(&flags);
    let orphan = inspect(&daemon).unwrap_or_else(|| panic!("no daemon"))["Id"].clone();
    docker_ok(&["kill", "-s", "KILL", &first]);
    assert!(exists(&daemon), "SIGKILL cannot have removed the daemon");

    let second = case.start("second", &flags);
    Case::wait_healthy(&second);
    let replacement = inspect(&daemon).unwrap_or_else(|| panic!("no daemon"))["Id"].clone();
    assert_ne!(orphan, replacement, "the orphan was kept");
    assert!(
        contains(&logs(&second), "removing an orphaned daemon"),
        "{}",
        logs(&second)
    );
    drop(case);
    session.finish();
}

/// A container with the daemon's name that the launcher did not make refuses the start and is
/// left untouched.
#[test]
fn a_foreign_container_with_the_daemons_name_is_untouched() {
    let Some(session) = begin("a_foreign_container_with_the_daemons_name_is_untouched") else {
        return;
    };
    let mut case = Case::new("foreign", 2);
    let flags = case.flags();
    let daemon = Case::daemon(&flags);
    let image = image();
    case.containers.push(daemon.clone());
    let foreign = docker_ok(&[
        "create",
        "--name",
        &daemon,
        "--network",
        "none",
        "--entrypoint",
        "true",
        &image,
    ]);
    let logs = case.refused("launcher", &flags);
    assert!(contains(&logs, "already in use"), "{logs}");
    let still = inspect(&daemon).unwrap_or_else(|| panic!("the foreign container was removed"));
    assert_eq!(still["Id"], foreign);
    drop(case);
    session.finish();
}
