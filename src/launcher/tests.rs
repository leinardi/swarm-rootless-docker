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
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::*;
use crate::config::{
    ExtraMount, HostPath, IdRange, InstanceName, LaunchValues, MemoryBytes, NetworkAlias,
    NetworkName, PidsLimit,
};
use crate::docker::MountInfo;
use crate::docker::fake::{self, Call, FakeDocker, Op, SIGTERM_EXIT, Script};
use crate::mounts::{Device, Placement};
use crate::probe::ProbeReport;
use crate::spec::{
    FileId, HostLocation, LABEL_IMAGE_GID, LABEL_IMAGE_UID, LABEL_ROLE, LOCK_NAME, ProbeRequest,
    ROLE_DAEMON, ROLE_PROBE, StorageIds,
};

const IMAGE_ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ENGINE_ROOT: &str = "/mnt/.ix-apps/docker";
const HOST_SOCKET: &str = "/run/docker.sock";
const NO_LABELS: [(&str, &str); 0] = [];
/// How long a test waits for something the launcher does at once.
const PATIENCE: Duration = Duration::from_secs(5);

/// The fake engine's `n`th container ID: the fake numbers them in creation order.
fn nth(n: u64) -> String {
    format!("{n:064x}")
}

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

fn herdquay() -> LaunchConfig {
    config("herdquay", "100000:65536", "herdquay")
}

/// A node: the launcher image and the engine's data root.
fn node() -> FakeDocker {
    let docker = FakeDocker::default();
    docker.add_image(
        IMAGE_ID,
        &[(LABEL_IMAGE_UID, "568"), (LABEL_IMAGE_GID, "568")],
    );
    docker.set_docker_root(ENGINE_ROOT);
    docker
}

/// A running launcher container, from the image, with the host socket mounted.
fn add_launcher(docker: &FakeDocker, name: &str) -> ContainerId {
    let id = docker.add(name, NO_LABELS, fake::Status::Running);
    docker.change(&id, |container| {
        container.image = Some(IMAGE_ID.to_owned());
        container.mounts = vec![MountInfo {
            source: HOST_SOCKET.to_owned(),
            destination: DOCKER_SOCKET.to_owned(),
        }];
    });
    ContainerId::parse(&id).unwrap()
}

/// `path` at its own position on one filesystem, as file `inode`.
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

/// The probe's report for `config`: inodes `base`, `base + 1` and `base + 2`.
fn accepted(config: &LaunchConfig, base: u64) -> ProbeReport {
    ProbeReport::Accepted {
        data_root: location(config.data_root().path().as_str(), base),
        socket_dir: location(config.socket_dir().path().as_str(), base + 1),
        mounts: vec![location(
            config.mounts()[0].source().path().as_str(),
            base + 2,
        )],
    }
}

fn storage(config: &LaunchConfig, base: u64) -> StorageIds {
    let ProbeReport::Accepted {
        data_root,
        socket_dir,
        mounts,
    } = accepted(config, base)
    else {
        unreachable!();
    };
    StorageIds::new(config, data_root, socket_dir, mounts).unwrap()
}

/// The probe for `config` prints `stdout` and exits with `exit`.
fn probe_prints(docker: &FakeDocker, config: &LaunchConfig, stdout: &str, exit: Option<i64>) {
    docker.script(
        &spec::probe_name(config),
        Script {
            output: vec![Output::Stdout(stdout.as_bytes().to_vec())],
            exit,
            gate: None,
        },
    );
}

fn probe_reports(docker: &FakeDocker, config: &LaunchConfig, report: &ProbeReport, exit: i64) {
    let line = format!("{}\n", serde_json::to_string(report).unwrap());
    probe_prints(docker, config, &line, Some(exit));
}

/// The daemon for `config` prints a line on each stream and keeps running, once `gate` (if any)
/// lets it start.
fn daemon_runs(docker: &FakeDocker, config: &LaunchConfig, gate: Option<Arc<Notify>>) {
    docker.script(
        &spec::daemon_name(config),
        Script {
            output: vec![
                Output::Stdout(b"dockerd: API listen\n".to_vec()),
                Output::Stderr(b"dockerd: a warning\n".to_vec()),
            ],
            exit: None,
            gate,
        },
    );
}

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// The test's side of a launch.
struct Harness {
    status: watch::Receiver<Status>,
    stop: watch::Sender<bool>,
    console: Console,
    stdout: Captured,
    stderr: Captured,
}

fn launch<'run>(
    docker: &'run FakeDocker,
    config: &'run LaunchConfig,
    launcher: &ContainerId,
    timing: Timing,
) -> (Launch<'run, FakeDocker>, Harness) {
    let (stdout, stderr) = (Captured::default(), Captured::default());
    let console = Console::new(stdout.clone(), stderr.clone()).unwrap();
    launch_to(docker, config, launcher, timing, console, (stdout, stderr))
}

fn launch_to<'run>(
    docker: &'run FakeDocker,
    config: &'run LaunchConfig,
    launcher: &ContainerId,
    timing: Timing,
    console: Console,
    (stdout, stderr): (Captured, Captured),
) -> (Launch<'run, FakeDocker>, Harness) {
    let (status, status_receiver) = watch::channel(Status::Starting);
    let (stop, shutdown) = watch::channel(false);
    let launch = Launch {
        docker,
        config,
        launcher: launcher.clone(),
        console: console.clone(),
        status,
        shutdown,
        timing,
    };
    let harness = Harness {
        status: status_receiver,
        stop,
        console,
        stdout,
        stderr,
    };
    (launch, harness)
}

/// Waits, yielding, until `condition` holds.
async fn until(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

fn started(docker: &FakeDocker, id: &str) -> bool {
    docker.calls().contains(&Call::Start(id.to_owned()))
}

/// The containers left in the engine, by name.
fn left(docker: &FakeDocker) -> Vec<String> {
    docker
        .containers()
        .into_iter()
        .map(|container| container.name)
        .collect()
}

/// IDs, in creation order, of a launch on a fresh [`node`] whose launcher was added first.
const LOCK: u64 = 2;
const PROBE: u64 = 3;
const DAEMON: u64 = 4;

/// The whole run, in order: identify; under the lock survey, probe, create; release; upload,
/// attach, start, and the wait only after the start. The daemon's own status is the launcher's.
/// Go: `TestRun_CallOrder`, `TestRun_LongLivedDaemon`.
#[tokio::test]
async fn runs_in_order_and_returns_the_daemon_status() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        let created = docker.by_name(&spec::daemon_name(&config)).unwrap();
        assert_eq!(
            created.uploads,
            vec![(SUBID_DIR.to_owned(), spec::subid_archive(&config).unwrap())]
        );
        // A daemon that runs far longer than any call timeout keeps running.
        tokio::time::sleep(Duration::from_millis(50)).await;
        docker.exit(&daemon, 3);
    });
    assert_eq!(result.unwrap(), 3);

    let calls: Vec<Call> = docker
        .calls()
        .into_iter()
        .filter(|call| *call != Call::Inspect(daemon.clone()))
        .collect();
    let probe = nth(PROBE);
    let lock = nth(LOCK);
    assert_eq!(
        calls,
        vec![
            Call::Inspect(me.as_str().to_owned()),
            Call::InspectImage(IMAGE_ID.to_owned()),
            Call::DockerRoot,
            Call::Create(LOCK_NAME.to_owned()),
            Call::List {
                label: format!("{LABEL_ROLE}={ROLE_PROBE}"),
                all: true
            },
            Call::List {
                label: format!("{LABEL_ROLE}={ROLE_DAEMON}"),
                all: true
            },
            Call::Create(spec::probe_name(&config)),
            Call::Attach(probe.clone()),
            Call::Start(probe.clone()),
            Call::Wait(probe.clone()),
            Call::Remove(probe),
            Call::Create(spec::daemon_name(&config)),
            Call::Remove(lock),
            Call::Upload(daemon.clone()),
            Call::Attach(daemon.clone()),
            Call::Start(daemon.clone()),
            Call::Wait(daemon.clone()),
            Call::Remove(daemon),
        ]
    );
    assert_eq!(left(&docker), vec!["launcher"]);
    assert!(harness.console.flush(PATIENCE));
    assert!(harness.stdout.text().contains("dockerd: API listen"));
    assert!(harness.stderr.text().contains("dockerd: a warning"));
    assert_eq!(*harness.status.borrow(), Status::Stopping);
}

/// The probe gets the engine's data root and the host socket as denied paths; the daemon is the
/// fixed spec with the probe's locations in its labels.
#[tokio::test]
async fn the_probe_and_daemon_specs_are_the_fixed_ones() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        docker.exit(&daemon, 0);
    });
    assert_eq!(result.unwrap(), 0);

    let denied = [
        AbsolutePath::parse(ENGINE_ROOT).unwrap(),
        AbsolutePath::parse(HOST_SOCKET).unwrap(),
    ];
    let request = ProbeRequest::new(&config, &image(), &denied);
    assert_eq!(
        docker.created(),
        vec![
            spec::lock_spec(&image(), &me),
            spec::probe_spec(&config, &image(), &me, &request).unwrap(),
            spec::daemon_spec(&config, &image(), &me, &storage(&config, 10)).unwrap(),
        ]
    );
}

/// A refusal creates no daemon, and leaves neither the probe nor the lock behind.
#[tokio::test]
async fn a_probe_refusal_creates_no_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    let refused = ProbeReport::Refused {
        reason: "/mnt/herdquay: owned by uid 568, not 0".to_owned(),
    };
    probe_reports(&docker, &config, &refused, 1);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());

    let error = launch.run().await.unwrap_err();
    assert!(
        matches!(&error, LaunchError::ProbeRefused(reason) if reason.contains("owned by uid 568")),
        "{error}"
    );
    assert_eq!(left(&docker), vec!["launcher"]);
    assert!(
        !docker
            .calls()
            .contains(&Call::Create(spec::daemon_name(&config)))
    );
}

/// Only a report and an exit code that agree count: anything else is a probe failure, never an
/// acceptance.
#[tokio::test]
async fn the_report_and_the_exit_code_must_agree() {
    let config = herdquay();
    let accepted_line = format!(
        "{}\n",
        serde_json::to_string(&accepted(&config, 10)).unwrap()
    );
    let refused_line = format!(
        "{}\n",
        serde_json::to_string(&ProbeReport::Refused {
            reason: "no".to_owned()
        })
        .unwrap()
    );
    let unknown_field = accepted_line.replacen('{', "{\"extra\":1,", 2);
    let two_lines = format!("{accepted_line}{refused_line}");
    for (case, stdout, exit) in [
        ("accepted, exit 1", accepted_line.as_str(), 1),
        ("refused, exit 0", refused_line.as_str(), 0),
        ("accepted, exit 2", accepted_line.as_str(), 2),
        ("not JSON", "probe: oops\n", 0),
        ("no output", "", 0),
        ("an unknown field", unknown_field.as_str(), 0),
        ("two reports", two_lines.as_str(), 0),
    ] {
        let docker = node();
        let me = add_launcher(&docker, "launcher");
        probe_prints(&docker, &config, stdout, Some(exit));
        // Were the report wrongly taken, the daemon would start and end at once.
        docker.script(
            &spec::daemon_name(&config),
            Script {
                exit: Some(0),
                ..Script::default()
            },
        );
        let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
        let error = launch.run().await.unwrap_err();
        assert!(matches!(error, LaunchError::Probe(_)), "{case}: {error}");
        assert_eq!(left(&docker), vec!["launcher"], "{case}");
    }
}

/// A report that names the wrong number of mounts refuses too.
#[tokio::test]
async fn a_report_for_other_paths_is_refused() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    let report = ProbeReport::Accepted {
        data_root: location("/mnt/herdquay/data", 10),
        socket_dir: location("/mnt/herdquay/socket", 11),
        mounts: Vec::new(),
    };
    probe_reports(&docker, &config, &report, 0);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let error = launch.run().await.unwrap_err();
    assert!(
        matches!(error, LaunchError::Spec(SpecError::StorageCount { .. })),
        "{error}"
    );
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// A probe that never reports is given up at the probe timeout, and removed.
#[tokio::test(start_paused = true)]
async fn a_hung_probe_times_out_and_is_removed() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_prints(&docker, &config, "", None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let started_at = Instant::now();
    let error = launch.run().await.unwrap_err();
    assert!(
        matches!(&error, LaunchError::Probe(reason) if reason.contains("no report")),
        "{error}"
    );
    assert_eq!(started_at.elapsed(), Timing::default().probe);
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// A stop request does not cancel the locked section: the daemon is created, the lock released,
/// and only then the request is honoured: the daemon is removed, never started, and the launcher
/// exits 0.
#[tokio::test]
async fn a_stop_before_the_start_removes_the_unstarted_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, harness) = launch(&docker, &config, &me, Timing::default());
    harness.stop.send(true).unwrap();

    assert_eq!(launch.run().await.unwrap(), 0);
    let calls = docker.calls();
    let daemon = nth(DAEMON);
    let created = calls
        .iter()
        .position(|call| *call == Call::Create(spec::daemon_name(&config)))
        .unwrap();
    let released = calls
        .iter()
        .position(|call| *call == Call::Remove(nth(LOCK)))
        .unwrap();
    let removed = calls
        .iter()
        .position(|call| *call == Call::Remove(daemon.clone()))
        .unwrap();
    assert!(created < released && released < removed, "{calls:?}");
    assert!(!started(&docker, &daemon), "{calls:?}");
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// A failure between the create and the supervision removes the daemon. Go:
/// `TestRun_AttachErrorRemovesCreated`, `TestRun_StartErrorRemovesCreated`.
#[tokio::test]
async fn a_failed_start_removes_the_daemon() {
    for op in [Op::Upload, Op::Attach, Op::Start, Op::Wait] {
        let docker = node();
        let config = herdquay();
        let me = add_launcher(&docker, "launcher");
        probe_reports(&docker, &config, &accepted(&config, 10), 0);
        daemon_runs(&docker, &config, None);
        docker.fail(op, &nth(DAEMON), DockerError::Other("boom".to_owned()));
        let (launch, _harness) = launch(&docker, &config, &me, Timing::default());

        let error = launch.run().await.unwrap_err();
        assert!(
            matches!(error, LaunchError::Docker { .. }),
            "{op:?}: {error}"
        );
        assert_eq!(left(&docker), vec!["launcher"], "{op:?}");
    }
}

/// A daemon created under a lock that cannot be released is removed, not left running unlocked.
#[tokio::test]
async fn a_failed_release_removes_the_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    docker.fail(
        Op::Remove,
        &nth(LOCK),
        DockerError::Other("boom".to_owned()),
    );
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());

    let error = launch.run().await.unwrap_err();
    assert!(
        matches!(error, LaunchError::Node(NodeError::Docker { .. })),
        "{error}"
    );
    assert!(docker.by_name(&spec::daemon_name(&config)).is_none());
    assert!(!started(&docker, &nth(DAEMON)));
}

/// A stop request while the daemon runs stops it by ID, removes it, and returns its status.
/// Go: `TestRun_CancelStopsDaemon`.
#[tokio::test]
async fn a_stop_request_stops_and_removes_the_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        harness.stop.send(true).unwrap();
    });
    assert_eq!(result.unwrap(), u8::try_from(SIGTERM_EXIT).unwrap());
    let calls = docker.calls();
    let stop = calls
        .iter()
        .position(|call| *call == Call::Stop(daemon.clone()))
        .unwrap();
    let remove = calls
        .iter()
        .position(|call| *call == Call::Remove(daemon.clone()))
        .unwrap();
    assert!(stop < remove, "{calls:?}");
    assert_eq!(left(&docker), vec!["launcher"]);
    assert_eq!(*harness.status.borrow(), Status::Stopping);
}

/// A daemon that does not stop in time is removed by force, and the launcher fails. Go:
/// `TestRun_ShutdownBudgetExhausted`.
#[tokio::test(start_paused = true)]
async fn a_daemon_that_does_not_stop_is_removed_by_force() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let daemon = nth(DAEMON);
    // The stop is refused, so the daemon keeps running and its wait never answers.
    docker.fail(Op::Stop, &daemon, DockerError::Other("refused".to_owned()));
    let (launch, harness) = launch(&docker, &config, &me, Timing::default());

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        harness.stop.send(true).unwrap();
    });
    let error = result.unwrap_err();
    assert!(matches!(error, LaunchError::StopTimeout(_)), "{error}");
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// A stalled health read cannot consume time before the stop and cleanup budgets begin.
#[tokio::test(start_paused = true)]
async fn a_stalled_health_check_cannot_extend_the_shutdown_budget() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let daemon = nth(DAEMON);
    docker.gate_inspect(&daemon, Arc::new(Notify::new()));
    docker.fail(Op::Stop, &daemon, DockerError::Other("refused".to_owned()));
    let timing = Timing::default();
    let (launch, harness) = launch(&docker, &config, &me, timing);
    let run = launch.run();
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => panic!("launch ended before its health check: {result:?}"),
        () = until("the stalled health check", || {
            docker.calls().contains(&Call::Inspect(daemon.clone()))
        }) => {}
    }
    docker.hold_output(&daemon);
    let stopped_at = Instant::now();
    harness.stop.send(true).unwrap();
    let budget = timing.stop + timing.finish;
    let result = tokio::time::timeout(budget + Duration::from_secs(1), run)
        .await
        .expect("a health check must not delay shutdown");
    assert!(
        matches!(result, Err(LaunchError::StopTimeout(_))),
        "{result:?}"
    );
    assert_eq!(stopped_at.elapsed(), budget);
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// A stalled health read cannot hide the daemon's exit or a failed log stream.
#[tokio::test(start_paused = true)]
async fn a_stalled_health_check_does_not_hide_daemon_events() {
    enum Event {
        Exit,
        LogFailure,
    }

    for event in [Event::Exit, Event::LogFailure] {
        let docker = node();
        let config = herdquay();
        let me = add_launcher(&docker, "launcher");
        probe_reports(&docker, &config, &accepted(&config, 10), 0);
        daemon_runs(&docker, &config, None);
        let daemon = nth(DAEMON);
        docker.gate_inspect(&daemon, Arc::new(Notify::new()));
        let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
        let run = launch.run();
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("launch ended before its health check: {result:?}"),
            () = until("the stalled health check", || {
                docker.calls().contains(&Call::Inspect(daemon.clone()))
            }) => {}
        }
        match event {
            Event::Exit => docker.exit(&daemon, 7),
            Event::LogFailure => docker.fail_output(&daemon, DockerError::Timeout),
        }
        let result = tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .expect("a health check must not hide daemon events");
        match event {
            Event::Exit => assert_eq!(result.unwrap(), 7),
            Event::LogFailure => assert!(
                matches!(result, Err(LaunchError::LogStream(_))),
                "{result:?}"
            ),
        }
        assert_eq!(left(&docker), vec!["launcher"]);
    }
}

/// Error wrappers retain the original cause for diagnostics and downcasting.
#[test]
fn launcher_errors_preserve_their_sources() {
    use std::error::Error as _;

    for error in [
        LaunchError::Archive(io::Error::other("archive cause")),
        LaunchError::Setup(io::Error::other("setup cause")),
    ] {
        assert!(
            error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .is_some()
        );
    }
    let error = LaunchError::LogStream(DockerError::Timeout);
    assert_eq!(
        error.source().unwrap().downcast_ref::<DockerError>(),
        Some(&DockerError::Timeout)
    );
}

/// A broken log stream while the daemon runs stops it and fails. Go: `TestRun_LogStreamLost`.
#[tokio::test]
async fn a_lost_log_stream_stops_the_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        docker.fail_output(&daemon, DockerError::Other("connection reset".to_owned()));
    });
    let error = result.unwrap_err();
    assert!(matches!(error, LaunchError::LogStream(_)), "{error}");
    assert!(docker.calls().contains(&Call::Stop(daemon)));
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// The end of the log stream alone is no loss: the launcher waits for the exit. Go:
/// `TestRun_EOFBeforeExit`.
#[tokio::test]
async fn the_end_of_the_output_before_the_exit_is_not_a_loss() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        docker.cut_output(&daemon);
        tokio::time::sleep(Duration::from_millis(20)).await;
        docker.exit(&daemon, 0);
    });
    assert_eq!(result.unwrap(), 0);
    assert!(!docker.calls().contains(&Call::Stop(daemon)));
}

/// A failed wait stops and removes the daemon, and fails. Go: `TestRun_WaitStreamError`.
#[tokio::test]
async fn a_failed_wait_stops_the_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the wait is registered", || {
            docker.calls().contains(&Call::Wait(daemon.clone()))
        })
        .await;
        docker.fail_wait(&daemon, &DockerError::Other("stream reset".to_owned()));
    });
    let error = result.unwrap_err();
    assert!(matches!(error, LaunchError::Wait(_)), "{error}");
    assert!(docker.calls().contains(&Call::Stop(daemon)));
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// The status socket follows the daemon's Docker health.
#[tokio::test]
async fn the_status_follows_the_daemon_health() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let timing = Timing {
        health: Duration::from_millis(5),
        ..Timing::default()
    };
    let (launch, harness) = launch(&docker, &config, &me, timing);
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the first health check", || {
            *harness.status.borrow() == Status::Running { health: None }
        })
        .await;
        docker.change(&daemon, |container| {
            container.health = Some("healthy".to_owned());
        });
        until("the daemon is healthy", || {
            harness.status.borrow().line() == "healthy"
        })
        .await;
        docker.exit(&daemon, 0);
    });
    assert_eq!(result.unwrap(), 0);
}

/// The reservation barrier: launcher A's daemon is created and A has released the lock, but its
/// start is held. Launcher B, with an overlapping range, runs to completion meanwhile and must
/// refuse, naming A's daemon, which stays created and untouched. Then A goes on.
#[tokio::test]
async fn a_created_daemon_is_a_reservation_before_it_starts() {
    let docker = node();
    let first = herdquay();
    let second = config("runner", "100000:65536", "runner");
    let first_launcher = add_launcher(&docker, "first-launcher");
    let second_launcher = add_launcher(&docker, "second-launcher");
    probe_reports(&docker, &first, &accepted(&first, 10), 0);
    probe_reports(&docker, &second, &accepted(&second, 20), 0);
    let gate = Arc::new(Notify::new());
    daemon_runs(&docker, &first, Some(Arc::clone(&gate)));
    daemon_runs(&docker, &second, None);
    let (first_launch, _first_harness) =
        launch(&docker, &first, &first_launcher, Timing::default());
    let first_name = spec::daemon_name(&first);

    let (result, ()) = tokio::join!(first_launch.run(), async {
        until("the first daemon's start is called", || {
            docker
                .by_name(&first_name)
                .is_some_and(|daemon| started(&docker, &daemon.id))
        })
        .await;
        let held = docker.by_name(&first_name).unwrap();
        assert_eq!(held.status, fake::Status::Created);

        let (second_launch, _second_harness) =
            launch(&docker, &second, &second_launcher, Timing::default());
        let error = second_launch.run().await.unwrap_err();
        assert!(
            matches!(
                &error,
                LaunchError::Node(NodeError::Conflict { what: "--subuid", container, .. })
                    if *container == short(&held.id)
            ),
            "{error}"
        );
        assert_eq!(docker.by_name(&first_name).unwrap(), held);
        assert!(docker.by_name(&spec::daemon_name(&second)).is_none());

        gate.notify_one();
        until("the first daemon runs", || {
            docker.by_name(&first_name).unwrap().status == fake::Status::Running
        })
        .await;
        docker.exit(&held.id, 0);
    });
    assert_eq!(result.unwrap(), 0);
}

const NETWORK_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const BRIDGE_ID: &str = "3333333333333333333333333333333333333333333333333333333333333333";

/// `config` on `network`, with `aliases` there.
fn aliased(config: &LaunchConfig, network: &str, aliases: &[&str]) -> LaunchConfig {
    LaunchConfig::new(LaunchValues {
        instance: config.instance().clone(),
        subuid: config.subuid(),
        subgid: Some(config.subgid()),
        data_root: config.data_root().clone(),
        socket_dir: config.socket_dir().clone(),
        mounts: config.mounts().to_vec(),
        network: NetworkName::parse(network).unwrap(),
        aliases: aliases
            .iter()
            .map(|alias| NetworkAlias::parse(alias).unwrap())
            .collect(),
        memory: config.memory(),
        pids_limit: config.pids_limit(),
        cpus: config.cpus(),
    })
    .unwrap()
}

/// A live daemon's alias is refused to a second instance on the same network, though the two
/// launchers name the network differently (by name, by ID); nothing of the second is created.
#[tokio::test]
async fn an_alias_is_one_daemons_per_network() {
    let docker = node();
    docker.add_network("docker", NETWORK_ID, false);
    let first = aliased(&herdquay(), "docker", &["docker"]);
    let second = aliased(
        &config("runner", "200000:65536", "runner"),
        NETWORK_ID,
        &["docker"],
    );
    let first_launcher = add_launcher(&docker, "first-launcher");
    let second_launcher = add_launcher(&docker, "second-launcher");
    probe_reports(&docker, &first, &accepted(&first, 10), 0);
    probe_reports(&docker, &second, &accepted(&second, 20), 0);
    daemon_runs(&docker, &first, None);
    let (first_launch, _first_harness) =
        launch(&docker, &first, &first_launcher, Timing::default());
    let first_name = spec::daemon_name(&first);

    let (result, ()) = tokio::join!(first_launch.run(), async {
        until("the first daemon runs", || {
            docker
                .by_name(&first_name)
                .is_some_and(|daemon| daemon.status == fake::Status::Running)
        })
        .await;
        let held = docker.by_name(&first_name).unwrap();

        let (second_launch, _second_harness) =
            launch(&docker, &second, &second_launcher, Timing::default());
        let error = second_launch.run().await.unwrap_err();
        assert!(
            matches!(
                &error,
                LaunchError::Node(NodeError::Conflict { what: "--alias", container, .. })
                    if *container == short(&held.id)
            ),
            "{error}"
        );
        assert!(docker.by_name(&spec::daemon_name(&second)).is_none());
        assert!(docker.by_name(&spec::probe_name(&second)).is_none());
        docker.exit(&held.id, 0);
    });
    assert_eq!(result.unwrap(), 0);
}

/// Aliases on the default bridge, named by its ID, refuse before the probe. (Without aliases the
/// network is never inspected: the call list of `runs_in_order_and_returns_the_daemon_status`.)
#[tokio::test]
async fn aliases_on_the_default_bridge_are_refused() {
    let docker = node();
    docker.add_network("bridge", BRIDGE_ID, true);
    let config = aliased(&herdquay(), BRIDGE_ID, &["docker"]);
    let me = add_launcher(&docker, "launcher");
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());

    let error = launch.run().await.unwrap_err();
    assert!(
        matches!(
            &error,
            LaunchError::Node(NodeError::AliasOnDefaultBridge { .. })
        ),
        "{error}"
    );
    assert_eq!(left(&docker), vec!["launcher"]);
    assert!(
        !docker
            .calls()
            .contains(&Call::Create(spec::probe_name(&config)))
    );
}

/// Without the host socket mounted where the launcher reaches it, nothing is created.
#[tokio::test]
async fn the_host_socket_must_be_mounted() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    docker.change(me.as_str(), |container| container.mounts.clear());
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let error = launch.run().await.unwrap_err();
    assert!(matches!(error, LaunchError::SelfId(_)), "{error}");
    assert!(docker.created().is_empty());
}

/// A writer that blocks until the test lets it go: a stdout nobody reads.
struct Stuck(std::sync::mpsc::Receiver<()>);

impl Write for Stuck {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _ = self.0.recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A stdout that blocks, with more daemon output than any queue holds, stalls nothing but the
/// copy: the stop request is still seen, and the daemon stopped and removed in time.
#[tokio::test(start_paused = true)]
async fn a_blocked_stdout_does_not_stall_the_launcher() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (release, stuck) = std::sync::mpsc::channel();
    let console = Console::new(Stuck(stuck), Captured::default()).unwrap();
    let (launch, harness) = launch_to(
        &docker,
        &config,
        &me,
        Timing::default(),
        console,
        (Captured::default(), Captured::default()),
    );
    let daemon = nth(DAEMON);

    let started_at = Instant::now();
    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        for _ in 0..4 * crate::output::LANE_CAPACITY {
            docker.print(&daemon, Output::Stdout(b"line\n".to_vec()));
            tokio::task::yield_now().await;
        }
        harness.stop.send(true).unwrap();
    });
    assert_eq!(result.unwrap(), u8::try_from(SIGTERM_EXIT).unwrap());
    assert!(started_at.elapsed() <= Timing::default().stop + Timing::default().finish);
    assert_eq!(left(&docker), vec!["launcher"]);
    drop(release);
}

/// An attach stream that stays open after the daemon's exit cannot take the remove's time: the
/// daemon is removed and its status is the launcher's.
#[tokio::test(start_paused = true)]
async fn a_stalled_drain_still_removes_the_daemon() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, _harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        docker.hold_output(&daemon);
        docker.exit(&daemon, 7);
    });
    assert_eq!(result.unwrap(), 7);
    assert_eq!(left(&docker), vec!["launcher"]);
}

/// The same during a shutdown.
#[tokio::test(start_paused = true)]
async fn a_stalled_drain_does_not_fail_the_shutdown() {
    let docker = node();
    let config = herdquay();
    let me = add_launcher(&docker, "launcher");
    probe_reports(&docker, &config, &accepted(&config, 10), 0);
    daemon_runs(&docker, &config, None);
    let (launch, harness) = launch(&docker, &config, &me, Timing::default());
    let daemon = nth(DAEMON);

    let (result, ()) = tokio::join!(launch.run(), async {
        until("the daemon runs", || started(&docker, &daemon)).await;
        docker.hold_output(&daemon);
        harness.stop.send(true).unwrap();
    });
    assert_eq!(result.unwrap(), u8::try_from(SIGTERM_EXIT).unwrap());
    assert_eq!(left(&docker), vec!["launcher"]);
}

#[test]
fn exit_codes_never_turn_a_failure_into_success() {
    for (code, want) in [(0, 0), (3, 3), (143, 143), (255, 255), (256, 1), (-1, 1)] {
        assert_eq!(exit_code(code), want, "{code}");
    }
}

/// Go: `TestParseSelfID`.
#[test]
fn self_id_from_the_mount_table() {
    let me = "a".repeat(64);
    let other = "b".repeat(64);
    let line = |id: u64, root: &str, target: &str| {
        format!("{id} 1 8:2 {root} {target} rw,relatime - ext4 /dev/sda2 rw\n")
    };
    let root = "1 0 0:420 / / rw,relatime - overlay overlay rw\n";
    let docker_root = format!(
        "{root}{}{}{}",
        line(
            2,
            &format!("/var/lib/docker/containers/{me}/resolv.conf"),
            "/etc/resolv.conf"
        ),
        line(
            3,
            &format!("/var/lib/docker/containers/{me}/hostname"),
            "/etc/hostname"
        ),
        line(
            4,
            &format!("/var/lib/docker/containers/{me}/hosts"),
            "/etc/hosts"
        ),
    );
    let truenas = format!(
        "{root}{}",
        line(
            2,
            &format!("/ix-apps/docker/containers/{me}/hostname"),
            "/etc/hostname"
        )
    );
    let separate_fs = format!(
        "{root}{}",
        line(2, &format!("/containers/{me}/hosts"), "/etc/hosts")
    );
    for (case, table) in [
        ("docker data root", docker_root),
        ("TrueNAS data root", truenas),
        ("data root on its own filesystem", separate_fs),
    ] {
        assert_eq!(self_id(&table).unwrap().as_str(), me, "{case}");
    }

    let no_match = format!("{root}{}", line(2, "/etc/resolv.conf", "/etc/resolv.conf"));
    let other_target = format!(
        "{root}{}",
        line(
            2,
            &format!("/var/lib/docker/containers/{other}/hostname"),
            "/mnt/hostname"
        )
    );
    let other_file = format!(
        "{root}{}",
        line(
            2,
            &format!("/var/lib/docker/containers/{me}/config.v2.json"),
            "/etc/hostname"
        )
    );
    let short_id = format!(
        "{root}{}",
        line(
            2,
            "/var/lib/docker/containers/abc123/hostname",
            "/etc/hostname"
        )
    );
    let ambiguous = format!(
        "{root}{}{}",
        line(
            2,
            &format!("/var/lib/docker/containers/{me}/hostname"),
            "/etc/hostname"
        ),
        line(
            3,
            &format!("/var/lib/docker/containers/{other}/hosts"),
            "/etc/hosts"
        ),
    );
    for (case, table, want) in [
        ("no match", no_match, "no Docker container ID"),
        ("other target", other_target, "no Docker container ID"),
        ("other file", other_file, "no Docker container ID"),
        ("short ID", short_id, "no Docker container ID"),
        ("ambiguous", ambiguous, "more than one"),
    ] {
        let error = self_id(&table).unwrap_err();
        assert!(error.to_string().contains(want), "{case}: {error}");
    }
}
