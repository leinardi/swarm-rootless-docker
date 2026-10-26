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

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::config::{
    ExtraMount, HostPath, IdRange, InstanceName, LaunchValues, MemoryBytes, NetworkAlias,
    NetworkName, PidsLimit,
};
use crate::docker::fake::{Call, FakeDocker, Op, Status};
use crate::mounts::{self, Device, Placement};
use crate::spec::{
    FileId, HostLocation, LABEL_ALIASES, LABEL_IMAGE_GID, LABEL_IMAGE_UID, LABEL_NETWORK,
};

const IMAGE_ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NO_LABELS: [(&str, &str); 0] = [];

fn image() -> ImageIdentity {
    let labels = HashMap::from([
        (LABEL_IMAGE_UID.to_owned(), "568".to_owned()),
        (LABEL_IMAGE_GID.to_owned(), "568".to_owned()),
    ]);
    ImageIdentity::new(IMAGE_ID, &labels).unwrap()
}

/// A configuration whose host paths all sit under `/mnt/<root>`.
fn config(instance: &str, subuid: &str, root: &str) -> LaunchConfig {
    LaunchConfig::new(LaunchValues {
        instance: InstanceName::parse(instance).unwrap(),
        subuid: IdRange::parse(subuid).unwrap(),
        subgid: None,
        data_root: HostPath::parse(&format!("/mnt/{root}/data")).unwrap(),
        socket_dir: HostPath::parse(&format!("/mnt/{root}/socket")).unwrap(),
        mounts: vec![ExtraMount::parse(&format!("/mnt/{root}/work:/work")).unwrap()],
        network: NetworkName::parse("docker").unwrap(),
        aliases: Vec::new(),
        memory: MemoryBytes::parse("1g").unwrap(),
        pids_limit: PidsLimit::parse("1024").unwrap(),
        cpus: None,
    })
    .unwrap()
}

/// `path` at its own position on one filesystem, as file `inode`: a host with no bind mounts.
fn location(path: &str, inode: u64) -> HostLocation {
    HostLocation {
        placement: Placement::new(
            Device {
                major: 0,
                minor: 52,
            },
            path,
        )
        .unwrap(),
        file: FileId { device: 52, inode },
    }
}

/// Inodes `base`, `base + 1` and `base + 2` for the data root, socket and mount source.
fn storage(config: &LaunchConfig, base: u64) -> StorageIds {
    StorageIds::new(
        config,
        location(config.data_root().path().as_str(), base),
        location(config.socket_dir().path().as_str(), base + 1),
        vec![location(
            config.mounts()[0].source().path().as_str(),
            base + 2,
        )],
    )
    .unwrap()
}

/// A launcher container in the fake engine.
fn launcher(docker: &FakeDocker, status: Status) -> ContainerId {
    ContainerId::parse(&docker.add("launcher", NO_LABELS, status)).unwrap()
}

/// A daemon for `config`, owned by `owner`, as the launcher would label it.
fn add_daemon(
    docker: &FakeDocker,
    config: &LaunchConfig,
    owner: &ContainerId,
    base: u64,
    status: Status,
) -> String {
    let labels = spec::daemon_labels(config, owner, &storage(config, base)).unwrap();
    docker.add(&spec::daemon_name(config), labels, status)
}

fn herdquay() -> LaunchConfig {
    config("herdquay", "100000:65536", "herdquay")
}

fn runner() -> LaunchConfig {
    config("runner", "200000:65536", "runner")
}

fn removed(docker: &FakeDocker) -> Vec<String> {
    docker
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Remove(id) => Some(id),
            Call::List { .. }
            | Call::Inspect(_)
            | Call::Create(_)
            | Call::InspectImage(_)
            | Call::InspectNetwork(_)
            | Call::DockerRoot
            | Call::Upload(_)
            | Call::Attach(_)
            | Call::Wait(_)
            | Call::Start(_)
            | Call::Stop(_) => None,
        })
        .collect()
}

/// A daemon of this instance left by a previous launcher: removed only when that launcher is
/// provably gone or is this one; any doubt refuses and leaves it. Go: `TestRemoveStale` (the
/// sh-wrapper and foreign-container cases have no counterpart: only labelled daemons are listed,
/// and a same-named foreign container makes the create fail).
#[tokio::test]
async fn survey_removes_only_provable_orphans() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Want {
        Removed,
        InstanceRunning,
        OwnerInspect,
        OwnerState,
        NoOwner,
    }
    enum Owner {
        Missing,
        Exited,
        Running,
        InspectFails,
        NoState,
        Myself,
        NoLabel,
        BadLabel,
    }
    let cases = [
        ("launcher gone", Owner::Missing, Want::Removed),
        ("launcher exited", Owner::Exited, Want::Removed),
        ("launcher running", Owner::Running, Want::InstanceRunning),
        (
            "launcher inspect fails",
            Owner::InspectFails,
            Want::OwnerInspect,
        ),
        ("launcher state unknown", Owner::NoState, Want::OwnerState),
        ("earlier run of this launcher", Owner::Myself, Want::Removed),
        ("no owner label", Owner::NoLabel, Want::NoOwner),
        ("owner label not an ID", Owner::BadLabel, Want::NoOwner),
    ];
    for (name, owner, want) in cases {
        let docker = FakeDocker::default();
        let me = launcher(&docker, Status::Running);
        let config = herdquay();
        let owner_id = match owner {
            Owner::Myself => me.clone(),
            Owner::Missing => ContainerId::parse(&"f".repeat(64)).unwrap(),
            Owner::Exited => launcher(&docker, Status::Exited),
            Owner::Running | Owner::InspectFails | Owner::NoLabel | Owner::BadLabel => {
                launcher(&docker, Status::Running)
            }
            Owner::NoState => {
                let id = launcher(&docker, Status::Running);
                docker.drop_state(id.as_str());
                id
            }
        };
        let mut labels = spec::daemon_labels(&config, &owner_id, &storage(&config, 10)).unwrap();
        match owner {
            Owner::NoLabel => {
                labels.remove(LABEL_LAUNCHER);
            }
            Owner::BadLabel => {
                labels.insert(LABEL_LAUNCHER.to_owned(), "launcher".to_owned());
            }
            Owner::InspectFails => docker.fail(
                Op::Inspect,
                owner_id.as_str(),
                DockerError::Other("context deadline exceeded".to_owned()),
            ),
            Owner::Missing | Owner::Exited | Owner::Running | Owner::NoState | Owner::Myself => {}
        }
        let daemon = docker.add(&spec::daemon_name(&config), labels, Status::Running);

        let result = survey(&docker, &config, &me).await;
        let got = match &result {
            Ok(reservations) => {
                assert!(reservations.is_empty(), "{name}: {reservations:?}");
                Want::Removed
            }
            Err(NodeError::InstanceRunning { .. }) => Want::InstanceRunning,
            Err(NodeError::Docker { .. }) => Want::OwnerInspect,
            Err(NodeError::OwnerState { .. }) => Want::OwnerState,
            Err(NodeError::NoOwner { .. }) => Want::NoOwner,
            Err(error) => panic!("{name}: {error}"),
        };
        assert_eq!(got, want, "{name}: {result:?}");
        let want_removed = if want == Want::Removed {
            vec![daemon]
        } else {
            Vec::new()
        };
        assert_eq!(removed(&docker), want_removed, "{name}");
    }
}

/// Another instance's daemon with a live owner is a reservation, in any state: a created, never
/// started one included, which only a list with `all` shows.
#[tokio::test]
async fn survey_returns_live_reservations_in_any_state() {
    for status in [Status::Created, Status::Running, Status::Exited] {
        let docker = FakeDocker::default();
        let me = launcher(&docker, Status::Running);
        let other = launcher(&docker, Status::Running);
        let daemon = add_daemon(&docker, &runner(), &other, 10, status);

        let reservations = survey(&docker, &herdquay(), &me).await.unwrap();
        assert_eq!(reservations.len(), 1, "{status:?}");
        assert_eq!(reservations[0].container, daemon);
        assert_eq!(reservations[0].held.instance.as_str(), "runner");
        assert!(removed(&docker).is_empty());
        for call in docker.calls() {
            if let Call::List { all, .. } = call {
                assert!(all, "a list without all hides created daemons");
            }
        }
    }
}

/// A live reservation whose labels do not parse refuses: its holdings are unknown.
#[tokio::test]
async fn survey_refuses_unreadable_reservations() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let other = launcher(&docker, Status::Running);
    let runner = runner();
    let mut labels = spec::daemon_labels(&runner, &other, &storage(&runner, 10)).unwrap();
    labels.insert(spec::LABEL_STORAGE.to_owned(), "[]".to_owned());
    docker.add(&spec::daemon_name(&runner), labels, Status::Running);

    let error = survey(&docker, &herdquay(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::Labels { .. }), "{error}");
    assert!(removed(&docker).is_empty());
}

/// Probes left by a gone launcher (or this one) are removed; a live launcher's are left alone
/// and are no reservation. Containers with no role label are never listed, so never touched.
#[tokio::test]
async fn survey_removes_stale_probes() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let live = launcher(&docker, Status::Running);
    let dead = launcher(&docker, Status::Exited);
    let probe = |owner: &ContainerId| {
        [
            (LABEL_ROLE, ROLE_PROBE.to_owned()),
            (LABEL_LAUNCHER, owner.as_str().to_owned()),
        ]
    };
    let stale = docker.add(
        "swarm-rootless-docker-probe-a",
        probe(&dead),
        Status::Exited,
    );
    let mine = docker.add("swarm-rootless-docker-probe-b", probe(&me), Status::Created);
    docker.add(
        "swarm-rootless-docker-probe-c",
        probe(&live),
        Status::Running,
    );
    docker.add(
        "swarm-rootless-docker-daemon-herdquay",
        NO_LABELS,
        Status::Running,
    );

    let reservations = survey(&docker, &herdquay(), &me).await.unwrap();
    assert!(reservations.is_empty());
    assert_eq!(removed(&docker), [stale, mine]);
}

/// Every overlap with a reservation refuses, naming it; disjoint configurations pass. UIDs and
/// GIDs are separate spaces: a subuid range may overlap another daemon's subgid range.
#[test]
fn conflicts() {
    let other = ContainerId::parse(&"e".repeat(64)).unwrap();
    let reserve = |config: &LaunchConfig, base| Reservation {
        container: "d".repeat(64),
        held: DaemonReservation::from_labels(
            &spec::daemon_labels(config, &other, &storage(config, base)).unwrap(),
        )
        .unwrap(),
        network_id: None,
    };
    let reservations = [reserve(&runner(), 10)];
    let ok = |config: &LaunchConfig, base| {
        check_conflicts(config, None, &storage(config, base), &reservations)
    };

    ok(&herdquay(), 20).unwrap();
    // UIDs and GIDs are separate: our subuid range may be their subgid range.
    let theirs = LaunchConfig::new(LaunchValues {
        subgid: Some(IdRange::parse("300000:65536").unwrap()),
        ..values_of(&runner())
    })
    .unwrap();
    let ours = LaunchConfig::new(LaunchValues {
        subuid: IdRange::parse("300000:65536").unwrap(),
        subgid: Some(IdRange::parse("100000:65536").unwrap()),
        ..values_of(&herdquay())
    })
    .unwrap();
    check_conflicts(&ours, None, &storage(&ours, 20), &[reserve(&theirs, 10)]).unwrap();

    let cases: [(&str, LaunchConfig, u64, &str); 6] = [
        (
            "subuid overlaps",
            config("herdquay", "265535:65536", "herdquay"),
            20,
            "--subuid",
        ),
        (
            "same data root",
            config("herdquay", "100000:65536", "runner"),
            20,
            "host path",
        ),
        (
            "nested in their tree",
            config("herdquay", "100000:65536", "runner/data/inner"),
            20,
            "host path",
        ),
        (
            "around their tree",
            LaunchConfig::new(LaunchValues {
                mounts: vec![ExtraMount::parse("/mnt/runner:/work").unwrap()],
                ..values_of(&herdquay())
            })
            .unwrap(),
            20,
            "host path",
        ),
        ("same inode, other path", herdquay(), 10, "host path"),
        (
            "same inode as their mount source",
            herdquay(),
            12,
            "host path",
        ),
    ];
    for (name, config, base, want) in cases {
        let error = ok(&config, base).unwrap_err();
        assert!(
            matches!(&error, NodeError::Conflict { what, instance, .. } if *what == want && instance == "runner"),
            "{name}: {error}"
        );
    }
}

/// A mount source that is a bind mount of a descendant of another daemon's tree conflicts,
/// though its path, its inode and every inode on its way differ from what that daemon holds.
/// The locations come from the mount table, as the probe will place them.
#[test]
fn conflict_through_a_bind_mount() {
    let table = mounts::parse(mounts::tests::TABLE).unwrap();
    let on_host = |path: &str, inode| HostLocation {
        placement: mounts::place(&table, &format!("/host{path}")).unwrap(),
        file: FileId { device: 1, inode },
    };
    let with_mount = |config: &LaunchConfig, mount: &str| {
        LaunchConfig::new(LaunchValues {
            mounts: vec![ExtraMount::parse(mount).unwrap()],
            ..values_of(config)
        })
        .unwrap()
    };
    let theirs = with_mount(&runner(), "/srv/work/tree:/work");
    let theirs_storage = StorageIds::new(
        &theirs,
        on_host("/mnt/runner/data", 10),
        on_host("/mnt/runner/socket", 11),
        vec![on_host("/srv/work/tree", 12)],
    )
    .unwrap();
    let other = ContainerId::parse(&"e".repeat(64)).unwrap();
    let reservations = [Reservation {
        container: "d".repeat(64),
        held: DaemonReservation::from_labels(
            &spec::daemon_labels(&theirs, &other, &theirs_storage).unwrap(),
        )
        .unwrap(),
        network_id: None,
    }];
    let ours_with = |mount_source: &str| {
        let ours = with_mount(&herdquay(), &format!("{mount_source}:/work"));
        let storage = StorageIds::new(
            &ours,
            on_host("/mnt/herdquay/data", 20),
            on_host("/mnt/herdquay/socket", 21),
            vec![on_host(mount_source, 22)],
        )
        .unwrap();
        check_conflicts(&ours, None, &storage, &reservations)
    };

    // /mnt/alias is a bind of /srv/work/tree/subdir.
    let error = ours_with("/mnt/alias").unwrap_err();
    assert!(
        matches!(
            &error,
            NodeError::Conflict {
                what: "host path",
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("bind mount"), "{error}");
    ours_with("/srv/work/other").unwrap();
}

fn values_of(config: &LaunchConfig) -> LaunchValues {
    LaunchValues {
        instance: config.instance().clone(),
        subuid: config.subuid(),
        subgid: Some(config.subgid()),
        data_root: config.data_root().clone(),
        socket_dir: config.socket_dir().clone(),
        mounts: config.mounts().to_vec(),
        network: config.network().clone(),
        aliases: config.aliases().to_vec(),
        memory: config.memory(),
        pids_limit: config.pids_limit(),
        cpus: config.cpus(),
    }
}

/// A free lock is created with the lock spec and released by ID.
#[tokio::test]
async fn lock_free() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let lock = NodeLock::acquire(&docker, &image(), &me).await.unwrap();
    let held = docker.by_name(LOCK_NAME).unwrap();
    assert_eq!(held.spec, Some(spec::lock_spec(&image(), &me)));
    assert_eq!(held.status, Status::Created);
    lock.release(&docker).await.unwrap();
    assert!(docker.by_name(LOCK_NAME).is_none());
    assert_eq!(removed(&docker), [held.id]);
}

/// A lock whose holder is gone, has exited, or is this launcher is stale: removed, then taken.
#[tokio::test]
async fn lock_stale_is_recovered() {
    for holder_status in [None, Some(Status::Exited), Some(Status::Running)] {
        let docker = FakeDocker::default();
        let me = launcher(&docker, Status::Running);
        let holder = match holder_status {
            None => ContainerId::parse(&"f".repeat(64)).unwrap(),
            Some(Status::Running) => me.clone(),
            Some(status) => launcher(&docker, status),
        };
        let stale = docker.add(LOCK_NAME, lock_labels(&holder), Status::Created);

        let lock = NodeLock::acquire(&docker, &image(), &me).await.unwrap();
        assert_eq!(removed(&docker), [stale], "{holder_status:?}");
        assert_eq!(
            docker.by_name(LOCK_NAME).unwrap().labels[LABEL_LAUNCHER],
            me.as_str()
        );
        lock.release(&docker).await.unwrap();
    }
}

fn lock_labels(owner: &ContainerId) -> [(&'static str, String); 2] {
    [
        (LABEL_ROLE, ROLE_LOCK.to_owned()),
        (LABEL_LAUNCHER, owner.as_str().to_owned()),
    ]
}

/// A lock held by a running launcher is waited for: taken once released, or refused after
/// [`LOCK_WAIT`], never removed.
#[tokio::test(start_paused = true)]
async fn lock_held_by_running_launcher() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let other = launcher(&docker, Status::Running);
    let held = docker.add(LOCK_NAME, lock_labels(&other), Status::Created);

    let started = Instant::now();
    let error = NodeLock::acquire(&docker, &image(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::LockBusy { .. }), "{error}");
    assert!(started.elapsed() >= LOCK_WAIT);
    assert!(started.elapsed() < LOCK_WAIT + Duration::from_secs(2));
    assert!(removed(&docker).is_empty());

    // Released while waiting: taken.
    let releaser = docker.clone();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        releaser.remove_container(&held).await.unwrap();
    });
    let lock = NodeLock::acquire(&docker, &image(), &me).await.unwrap();
    release.await.unwrap();
    assert_eq!(
        docker.by_name(LOCK_NAME).unwrap().labels[LABEL_LAUNCHER],
        me.as_str()
    );
    lock.release(&docker).await.unwrap();
}

/// A lock name held by something that is not a lock, or by a lock whose holder cannot be judged,
/// refuses and leaves it alone.
#[tokio::test]
async fn lock_refuses_what_it_cannot_judge() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    docker.add(LOCK_NAME, NO_LABELS, Status::Running);
    let error = NodeLock::acquire(&docker, &image(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::LockForeign { .. }), "{error}");

    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    docker.add(LOCK_NAME, [(LABEL_ROLE, ROLE_LOCK)], Status::Created);
    let error = NodeLock::acquire(&docker, &image(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::NoOwner { .. }), "{error}");

    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let other = launcher(&docker, Status::Running);
    docker.drop_state(other.as_str());
    docker.add(LOCK_NAME, lock_labels(&other), Status::Created);
    let error = NodeLock::acquire(&docker, &image(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::OwnerState { .. }), "{error}");

    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    docker.fail(
        Op::Create,
        LOCK_NAME,
        DockerError::Other("engine down".to_owned()),
    );
    let error = NodeLock::acquire(&docker, &image(), &me).await.unwrap_err();
    assert!(matches!(error, NodeError::Docker { .. }), "{error}");
    assert!(removed(&docker).is_empty());
}

/// Released between the create's conflict and the inspect: the create is simply retried.
#[tokio::test]
async fn lock_released_during_acquire() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    docker.fail(
        Op::Create,
        LOCK_NAME,
        DockerError::Conflict("in use".to_owned()),
    );
    let lock = NodeLock::acquire(&docker, &image(), &me).await.unwrap();
    assert_eq!(
        docker.calls()[..3],
        [
            Call::Create(LOCK_NAME.to_owned()),
            Call::Inspect(LOCK_NAME.to_owned()),
            Call::Create(LOCK_NAME.to_owned()),
        ]
    );
    lock.release(&docker).await.unwrap();
}

/// The lock is released when the work fails, and the work's error is returned.
#[tokio::test]
async fn with_node_lock_releases_on_error() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let result: Result<(), NodeError> = with_node_lock(&docker, &image(), &me, async || {
        assert!(docker.by_name(LOCK_NAME).is_some());
        Err(NodeError::LockForeign {
            container: "work".to_owned(),
        })
    })
    .await;
    assert!(matches!(result, Err(NodeError::LockForeign { container }) if container == "work"));
    assert!(docker.by_name(LOCK_NAME).is_none());
}

/// A lock someone else already removed releases cleanly.
#[tokio::test]
async fn release_of_a_removed_lock() {
    let docker = FakeDocker::default();
    let me = launcher(&docker, Status::Running);
    let lock = NodeLock::acquire(&docker, &image(), &me).await.unwrap();
    let id = docker.by_name(LOCK_NAME).unwrap().id;
    docker.remove_container(&id).await.unwrap();
    lock.release(&docker).await.unwrap();
}

/// Launchers started together on one node run their critical sections one at a time, and all
/// finish: each yields inside, so without the lock they would interleave.
#[tokio::test(start_paused = true)]
async fn lock_serializes_concurrent_launchers() {
    const LAUNCHERS: usize = 6;
    let docker = FakeDocker::default();
    let inside = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..LAUNCHERS {
        let docker = docker.clone();
        let me = launcher(&docker, Status::Running);
        let (inside, most) = (Arc::clone(&inside), Arc::clone(&most));
        tasks.push(tokio::spawn(async move {
            with_node_lock(&docker, &image(), &me, async || {
                let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                for _ in 0..3 {
                    tokio::task::yield_now().await;
                }
                inside.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, NodeError>(())
            })
            .await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(most.load(Ordering::SeqCst), 1);
    assert!(docker.by_name(LOCK_NAME).is_none());
}

/// The labels the launcher writes read back as what it holds.
#[test]
fn reservation_round_trip() {
    let config = herdquay();
    let owner = ContainerId::parse(&"e".repeat(64)).unwrap();
    let labels = spec::daemon_labels(&config, &owner, &storage(&config, 10)).unwrap();
    let held = DaemonReservation::from_labels(&labels).unwrap();
    assert_eq!(held.instance, *config.instance());
    assert_eq!(
        (held.subuid, held.subgid),
        (config.subuid(), config.subgid())
    );
    let want: Vec<_> = storage(&config, 10)
        .entries(&config)
        .into_iter()
        .map(|(path, location)| (path.clone(), location.clone()))
        .collect();
    assert_eq!(held.storage, want);
}

const NETWORK_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const OTHER_NETWORK_ID: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const BRIDGE_ID: &str = "3333333333333333333333333333333333333333333333333333333333333333";

/// `config` on `network`, with `aliases` there.
fn aliased(config: &LaunchConfig, network: &str, aliases: &[&str]) -> LaunchConfig {
    LaunchConfig::new(LaunchValues {
        network: NetworkName::parse(network).unwrap(),
        aliases: aliases
            .iter()
            .map(|alias| NetworkAlias::parse(alias).unwrap())
            .collect(),
        ..values_of(config)
    })
    .unwrap()
}

/// The engine of these tests: `docker` and `other` user-defined, and the default bridge.
fn with_networks() -> FakeDocker {
    let docker = FakeDocker::default();
    docker.add_network("docker", NETWORK_ID, false);
    docker.add_network("other", OTHER_NETWORK_ID, false);
    docker.add_network("bridge", BRIDGE_ID, true);
    docker
}

fn network_calls(docker: &FakeDocker) -> Vec<String> {
    docker
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::InspectNetwork(network) => Some(network),
            Call::List { .. }
            | Call::Inspect(_)
            | Call::Create(_)
            | Call::Remove(_)
            | Call::InspectImage(_)
            | Call::DockerRoot
            | Call::Upload(_)
            | Call::Attach(_)
            | Call::Wait(_)
            | Call::Start(_)
            | Call::Stop(_) => None,
        })
        .collect()
}

/// The network and aliases read back as written; a daemon from before those labels has neither,
/// and one of the two labels without the other, or a bad value, refuses.
#[test]
fn network_labels_round_trip() {
    let config = aliased(&herdquay(), "docker", &["docker", "dind"]);
    let owner = ContainerId::parse(&"e".repeat(64)).unwrap();
    let labels = spec::daemon_labels(&config, &owner, &storage(&config, 10)).unwrap();
    let held = DaemonReservation::from_labels(&labels).unwrap();
    assert_eq!(held.network.as_ref(), Some(config.network()));
    assert_eq!(held.aliases, config.aliases());

    let without = |removed: &[&str]| {
        let mut labels = labels.clone();
        for label in removed {
            labels.remove(*label);
        }
        DaemonReservation::from_labels(&labels)
    };
    let legacy = without(&[LABEL_NETWORK, LABEL_ALIASES]).unwrap();
    assert_eq!((legacy.network, legacy.aliases), (None, Vec::new()));
    assert_eq!(
        without(&[LABEL_ALIASES]).unwrap_err(),
        LabelError::Missing(LABEL_ALIASES)
    );
    assert_eq!(
        without(&[LABEL_NETWORK]).unwrap_err(),
        LabelError::Missing(LABEL_NETWORK)
    );

    for (label, value) in [
        (LABEL_ALIASES, "docker,Dind"),
        (LABEL_ALIASES, "docker,"),
        (LABEL_ALIASES, "swarm-rootless-docker-daemon-runner"),
        (LABEL_ALIASES, "a,b,c,d,e,f,g,h,i"),
        (LABEL_NETWORK, "host"),
    ] {
        let mut labels = labels.clone();
        labels.insert(label.to_owned(), value.to_owned());
        assert!(
            matches!(
                DaemonReservation::from_labels(&labels),
                Err(LabelError::Invalid { label: found, .. }) if found == label
            ),
            "{label}={value}"
        );
    }
}

/// A shared alias conflicts unless both networks resolved to different IDs: a network that did
/// not resolve may be the other one.
#[test]
fn alias_conflicts() {
    let other = ContainerId::parse(&"e".repeat(64)).unwrap();
    let reserve = |config: &LaunchConfig, network_id: Option<&str>| Reservation {
        container: "d".repeat(64),
        held: DaemonReservation::from_labels(
            &spec::daemon_labels(config, &other, &storage(config, 10)).unwrap(),
        )
        .unwrap(),
        network_id: network_id.map(ToOwned::to_owned),
    };
    let theirs = aliased(&runner(), "docker", &["docker", "dind"]);
    let check = |ours: &LaunchConfig, mine: Option<&str>, theirs_id: Option<&str>| {
        check_conflicts(
            ours,
            mine,
            &storage(ours, 20),
            &[reserve(&theirs, theirs_id)],
        )
    };

    let shared = aliased(&herdquay(), "docker", &["build", "dind"]);
    for (mine, theirs_id) in [
        (Some(NETWORK_ID), Some(NETWORK_ID)),
        (Some(NETWORK_ID), None),
        (None, Some(NETWORK_ID)),
        (None, None),
    ] {
        let error = check(&shared, mine, theirs_id).unwrap_err();
        assert!(
            matches!(&error, NodeError::Conflict { what: "--alias", instance, .. } if instance == "runner"),
            "{mine:?} {theirs_id:?}: {error}"
        );
        assert!(error.to_string().contains("dind"), "{error}");
    }
    check(&shared, Some(NETWORK_ID), Some(OTHER_NETWORK_ID)).unwrap();
    check(&aliased(&herdquay(), "docker", &["build"]), None, None).unwrap();
    check(&herdquay(), None, None).unwrap();

    // A daemon from before the network labels has no aliases.
    let mut legacy = reserve(&runner(), None);
    legacy.held.network = None;
    check_conflicts(&shared, None, &storage(&shared, 20), &[legacy]).unwrap();
}

/// One network named by name, full ID or ID prefix is one network: the same alias on it
/// conflicts whichever way each launcher named it. Another network does not; one that does not
/// resolve here is not known to be another.
#[tokio::test]
async fn alias_conflicts_compare_resolved_networks() {
    for (network, conflicts) in [
        ("docker", true),
        (NETWORK_ID, true),
        (&NETWORK_ID[..12], true),
        ("other", false),
        (OTHER_NETWORK_ID, false),
        ("absent", true),
    ] {
        let docker = with_networks();
        let me = launcher(&docker, Status::Running);
        let other = launcher(&docker, Status::Running);
        add_daemon(
            &docker,
            &aliased(&runner(), "docker", &["docker"]),
            &other,
            10,
            Status::Running,
        );
        let ours = aliased(&herdquay(), network, &["docker"]);

        let reservations = survey(&docker, &ours, &me).await.unwrap();
        let network_id = resolve_alias_network(&docker, &ours).await.unwrap();
        let result = check_conflicts(
            &ours,
            network_id.as_deref(),
            &storage(&ours, 20),
            &reservations,
        );
        assert_eq!(result.is_err(), conflicts, "{network}: {result:?}");
    }
}

/// Networks are resolved only when an alias could conflict: both sides need one.
#[tokio::test]
async fn networks_resolve_only_with_aliases() {
    let docker = with_networks();
    let me = launcher(&docker, Status::Running);
    let other = launcher(&docker, Status::Running);
    add_daemon(
        &docker,
        &aliased(&runner(), "other", &["docker"]),
        &other,
        10,
        Status::Running,
    );

    survey(&docker, &herdquay(), &me).await.unwrap();
    assert_eq!(
        resolve_alias_network(&docker, &herdquay()).await.unwrap(),
        None
    );
    assert!(network_calls(&docker).is_empty());

    let ours = aliased(&herdquay(), "docker", &["build"]);
    let reservations = survey(&docker, &ours, &me).await.unwrap();
    assert_eq!(
        reservations[0].network_id.as_deref(),
        Some(OTHER_NETWORK_ID)
    );
    assert_eq!(network_calls(&docker), ["other"]);
}

/// The default bridge refuses aliases whatever it is called by; an inspect that fails other than
/// not found refuses too.
#[tokio::test]
async fn alias_network_resolution_fails_closed() {
    let docker = with_networks();
    for network in ["bridge", BRIDGE_ID, &BRIDGE_ID[..12]] {
        let values = LaunchValues {
            network: NetworkName::parse(network).unwrap(),
            aliases: vec![NetworkAlias::parse("docker").unwrap()],
            ..values_of(&herdquay())
        };
        // `--network bridge` is refused lexically already.
        let Ok(config) = LaunchConfig::new(values) else {
            assert_eq!(network, "bridge");
            continue;
        };
        let error = resolve_alias_network(&docker, &config).await.unwrap_err();
        assert!(
            matches!(&error, NodeError::AliasOnDefaultBridge { .. }),
            "{network}: {error}"
        );
    }

    let ours = aliased(&herdquay(), "docker", &["docker"]);
    assert_eq!(
        resolve_alias_network(&docker, &ours)
            .await
            .unwrap()
            .as_deref(),
        Some(NETWORK_ID)
    );
    docker.fail(
        Op::InspectNetwork,
        "",
        DockerError::Other("boom".to_owned()),
    );
    let error = resolve_alias_network(&docker, &ours).await.unwrap_err();
    assert!(matches!(&error, NodeError::Docker { .. }), "{error}");
}

/// Every name Docker's DNS registers for a daemon created from `spec`, but its random short ID:
/// the container name, the hostname and the aliases.
fn registered_names(spec: &spec::ContainerSpec) -> Vec<String> {
    let mut names = vec![spec.name.clone()];
    names.extend(spec.body.hostname.clone());
    for endpoint in spec
        .body
        .networking_config
        .iter()
        .flat_map(|networking| networking.endpoints_config.iter().flat_map(HashMap::values))
    {
        names.extend(endpoint.aliases.clone().unwrap_or_default());
    }
    names
}

/// What a daemon registers is what the conflict check compares: [`spec::dns_names`].
#[test]
fn dns_names_are_what_docker_registers() {
    let image = image();
    let owner = ContainerId::parse(&"e".repeat(64)).unwrap();
    for config in [
        herdquay(),
        aliased(&herdquay(), "docker", &["docker", "dind"]),
    ] {
        let spec = spec::daemon_spec(&config, &image, &owner, &storage(&config, 10)).unwrap();
        let mut registered = registered_names(&spec);
        registered.dedup();
        assert_eq!(
            registered,
            spec::dns_names(config.instance(), config.aliases())
        );
    }
}

/// An instance's names never meet another's alias: instance `docker` without aliases and
/// instance `runner` with `--alias docker` share one network, in either launch order, and no
/// name either registers is the other's.
#[tokio::test]
async fn an_instance_name_is_never_an_alias() {
    let named_docker = config("docker", "300000:65536", "docker");
    let aliased_docker = aliased(&runner(), "docker", &["docker"]);
    for (first, second) in [
        (&named_docker, &aliased_docker),
        (&aliased_docker, &named_docker),
    ] {
        let docker = with_networks();
        let me = launcher(&docker, Status::Running);
        let other = launcher(&docker, Status::Running);
        add_daemon(&docker, first, &other, 10, Status::Running);

        let reservations = survey(&docker, second, &me).await.unwrap();
        let network_id = resolve_alias_network(&docker, second).await.unwrap();
        check_conflicts(
            second,
            network_id.as_deref(),
            &storage(second, 20),
            &reservations,
        )
        .unwrap();

        let image = image();
        let first_names = registered_names(
            &spec::daemon_spec(first, &image, &other, &storage(first, 10)).unwrap(),
        );
        let second_names = registered_names(
            &spec::daemon_spec(second, &image, &me, &storage(second, 20)).unwrap(),
        );
        assert!(
            first_names.iter().all(|name| !second_names.contains(name)),
            "{first_names:?} and {second_names:?}"
        );
    }
}

/// The check compares every DNS name, not the aliases only: a reservation whose container name
/// is one of the candidate's names conflicts (labels as an older or foreign launcher could have
/// written them; ours cannot produce it).
#[test]
fn every_dns_name_is_compared() {
    let other = ContainerId::parse(&"e".repeat(64)).unwrap();
    let theirs = runner();
    let mut held = DaemonReservation::from_labels(
        &spec::daemon_labels(&theirs, &other, &storage(&theirs, 10)).unwrap(),
    )
    .unwrap();
    held.instance = InstanceName::parse("herdquay").unwrap();
    held.subuid = IdRange::parse("500000:65536").unwrap();
    held.subgid = held.subuid;
    held.storage.clear();
    let reservation = Reservation {
        container: "d".repeat(64),
        held,
        network_id: None,
    };
    let ours = herdquay();
    let error = check_conflicts(&ours, None, &storage(&ours, 20), &[reservation]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("swarm-rootless-docker-daemon-herdquay"),
        "{error}"
    );
}
