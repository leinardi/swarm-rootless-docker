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

//! What the launchers of one node share: overlapping instances, the created-not-started
//! reservation, launchers started together, and the node lock.

use std::time::Instant;

use swarm_rootless_docker::config::LaunchConfig;
use swarm_rootless_docker::mounts::{Device, Placement};
use swarm_rootless_docker::node::LOCK_WAIT;
use swarm_rootless_docker::spec::{
    self, ContainerId, FileId, HostLocation, LABEL_LAUNCHER, LABEL_ROLE, LOCK_NAME, ROLE_LOCK,
    StorageIds,
};

use crate::harness::*;

/// A second instance whose range overlaps a running one refuses, naming it.
#[test]
fn an_overlapping_instance_is_refused() {
    let Some(session) = begin("an_overlapping_instance_is_refused") else {
        return;
    };
    let mut first = Case::new("first", 3);
    let flags = first.flags();
    let launcher = first.start("launcher", &flags);
    Case::wait_healthy(&launcher);
    let held = inspect(&Case::daemon(&flags)).unwrap_or_else(|| panic!("no daemon"));
    let short: String = held["Id"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(12)
        .collect();

    let mut second = Case::new("second", 4);
    let overlapping = Flags {
        subuid: flags.subuid.clone(),
        ..second.flags()
    };
    let logs = second.refused("launcher", &overlapping);
    assert!(
        contains(&logs, "--subuid conflicts with daemon") && contains(&logs, &short),
        "{logs}"
    );

    let same_storage = Flags {
        data_root: flags.data_root.clone(),
        ..second.flags()
    };
    let logs = second.refused("storage", &same_storage);
    assert!(contains(&logs, "host path conflicts with daemon"), "{logs}");
    drop(second);
    drop(first);
    session.finish();
}

/// Location labels that cannot collide with a real host path: a device no host has.
fn standin_storage(config: &LaunchConfig) -> StorageIds {
    let location = |path: &str, inode: u64| HostLocation {
        placement: must(
            Placement::new(
                Device {
                    major: 4095,
                    minor: 4095,
                },
                path,
            ),
            "a placement",
        ),
        file: FileId {
            device: u64::MAX,
            inode,
        },
    };
    must(
        StorageIds::new(
            config,
            location(config.data_root().path().as_str(), 1),
            location(config.socket_dir().path().as_str(), 2),
            config
                .mounts()
                .iter()
                .zip(3..)
                .map(|(mount, inode)| location(mount.source().path().as_str(), inode))
                .collect(),
        ),
        "the stand-in storage",
    )
}

/// The created-not-started window, built by hand: launcher A (a running stand-in) has created
/// its daemon, labelled exactly as `daemon_labels` writes them, released the lock and not started
/// it yet. Launcher B with an overlapping range refuses, naming that container, which stays
/// created and untouched; B with its own range starts beside it.
#[test]
fn a_created_daemon_is_a_reservation_before_it_starts() {
    let Some(session) = begin("a_created_daemon_is_a_reservation_before_it_starts") else {
        return;
    };
    let mut held = Case::new("held", 6);
    let flags = held.flags();
    let image = image();
    let owner = held.track("srd-it-held-owner");
    let owner_id = docker_ok(&[
        "run",
        "-d",
        "--name",
        &owner,
        "--network",
        "none",
        "--entrypoint",
        "sleep",
        &image,
        "infinity",
    ]);
    let config = flags.config();
    let labels = must(
        spec::daemon_labels(
            &config,
            &must(ContainerId::parse(&owner_id), "the owner's ID"),
            &standin_storage(&config),
        ),
        "the daemon labels",
    );
    let daemon = Case::daemon(&flags);
    let label_args: Vec<String> = labels
        .iter()
        .flat_map(|(key, value)| ["--label".to_owned(), format!("{key}={value}")])
        .collect();
    let mut create = vec![
        "create",
        "--name",
        &daemon,
        "--network",
        "none",
        "--entrypoint",
        "true",
    ];
    create.extend(label_args.iter().map(String::as_str));
    create.push(&image);
    docker_ok(&create);
    let before = inspect(&daemon).unwrap_or_else(|| panic!("no stand-in daemon"));
    assert_eq!(before["State"]["Status"], "created");

    let mut contender = Case::new("contender", 7);
    let logs = contender.refused(
        "conflicting",
        &Flags {
            subuid: flags.subuid.clone(),
            ..contender.flags()
        },
    );
    assert!(
        contains(&logs, "--subuid conflicts with daemon") && contains(&logs, &short_id(&daemon)),
        "{logs}"
    );
    let after = inspect(&daemon).unwrap_or_else(|| panic!("the stand-in daemon was removed"));
    assert_eq!(after["State"]["Status"], "created");
    assert_eq!(after["Config"]["Labels"], before["Config"]["Labels"]);

    let free = contender.flags();
    let launcher = contender.start("free", &free);
    Case::wait_healthy(&launcher);
    drop(contender);
    drop(held);
    session.finish();
}

/// Launchers started together, each a different instance: with one shared range exactly one
/// daemon runs and the others refuse; with their own ranges every one runs. The node lock
/// serialises them either way.
#[test]
fn launchers_started_together_never_share_a_range() {
    let Some(session) = begin("launchers_started_together_never_share_a_range") else {
        return;
    };
    for (round, shared) in [(0, true), (1, false)] {
        let mut cases: Vec<Case> = (0..3)
            .map(|index| Case::new(&format!("race{round}{index}"), 8 + round * 3 + index))
            .collect();
        let range = cases[0].flags().subuid;
        let launchers: Vec<String> = cases
            .iter_mut()
            .map(|case| {
                let mut flags = case.flags();
                if shared {
                    flags.subuid.clone_from(&range);
                }
                case.start("launcher", &flags)
            })
            .collect();
        let outcomes: Vec<Outcome> = launchers.iter().map(|launcher| outcome(launcher)).collect();
        let healthy = outcomes
            .iter()
            .filter(|outcome| **outcome == Outcome::Healthy)
            .count();
        if shared {
            assert_eq!(healthy, 1, "shared range: {outcomes:?}");
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == Outcome::Exited(1))
                    .count(),
                2,
                "shared range: {outcomes:?}"
            );
        } else {
            assert_eq!(healthy, 3, "own ranges: {outcomes:?}");
        }
        drop(cases);
    }
    session.finish();
}

/// A node lock whose launcher is gone (created, never run: not running) is stale: the next
/// launcher removes it and starts.
#[test]
fn a_lock_left_by_a_dead_launcher_is_removed() {
    let Some(session) = begin("a_lock_left_by_a_dead_launcher_is_removed") else {
        return;
    };
    let mut case = Case::new("stale", 14);
    let image = image();
    let owner = case.track("srd-it-stale-owner");
    let owner_id = docker_ok(&[
        "create",
        "--name",
        &owner,
        "--network",
        "none",
        "--entrypoint",
        "true",
        &image,
    ]);
    let lock = case.track(LOCK_NAME);
    create_lock(&lock, &owner_id, &image);

    let launcher = case.start("launcher", &case.flags());
    Case::wait_healthy(&launcher);
    assert!(
        contains(&logs(&launcher), "removing a stale node lock"),
        "{}",
        logs(&launcher)
    );
    assert!(!exists(&lock), "the stale lock is still there");
    drop(case);
    session.finish();
}

/// A node lock held by a running launcher is waited for, then given up: the launcher exits 1
/// after the lock wait and leaves the lock alone.
#[test]
fn a_lock_held_by_a_running_launcher_is_never_taken() {
    let Some(session) = begin("a_lock_held_by_a_running_launcher_is_never_taken") else {
        return;
    };
    let mut case = Case::new("busy", 15);
    let image = image();
    let owner = case.track("srd-it-busy-owner");
    let owner_id = docker_ok(&[
        "run",
        "-d",
        "--name",
        &owner,
        "--network",
        "none",
        "--entrypoint",
        "sleep",
        &image,
        "infinity",
    ]);
    let lock = case.track(LOCK_NAME);
    create_lock(&lock, &owner_id, &image);
    let held = short_id(&lock);

    let started = Instant::now();
    let logs = case.refused("launcher", &case.flags());
    assert!(
        started.elapsed() >= LOCK_WAIT,
        "gave up after {:?}",
        started.elapsed()
    );
    assert!(
        contains(&logs, "the node lock is held by running launcher"),
        "{logs}"
    );
    assert_eq!(short_id(&lock), held, "the held lock was replaced");
    drop(case);
    session.finish();
}

/// A never-started node lock owned by `owner_id`, as a launcher creates it.
fn create_lock(name: &str, owner_id: &str, image: &str) {
    let role = format!("{LABEL_ROLE}={ROLE_LOCK}");
    let launcher = format!("{LABEL_LAUNCHER}={owner_id}");
    docker_ok(&[
        "create",
        "--name",
        name,
        "--label",
        &role,
        "--label",
        &launcher,
        "--network",
        "none",
        "--entrypoint",
        "true",
        image,
    ]);
}
