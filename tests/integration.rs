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

//! Integration tests: the built image against the machine's own Docker, the real launcher, probe
//! and rootless daemon.
//!
//! Off unless `SRD_IT=1`. They need the image (`make image-build`; `SRD_IT_IMAGE` names another),
//! the `docker` CLI on the host socket, and unprivileged user namespaces outside AppArmor's
//! restriction (`kernel.apparmor_restrict_unprivileged_userns=0`). CI also sets
//! `SRD_IT_REQUIRE=1`, so tests that did not run fail instead of passing.
//!
//! The host paths live under `SRD_IT_ROOT` (default `/srv/srd-it`), whose parent must be
//! root-owned and not writable by others. Root-owned directories there are made through a helper
//! container on the host Docker, so the tests need no sudo. One test runs at a time: they share
//! the node, its lock and its volume list.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::{self, Write as _};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const DEFAULT_IMAGE: &str = "ghcr.io/leinardi/swarm-rootless-docker:dev-uid568";
const DEFAULT_ROOT: &str = "/srv/srd-it";
const NETWORK: &str = "srd-it";
const UID_TEXT: &str = "568";
const HOST_SOCKET: &str = "/var/run/docker.sock";
const INNER_IMAGE: &str = "busybox:1.37";
const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
/// A launcher must be healthy within the image's healthcheck start period.
const HEALTHY_WITHIN: Duration = Duration::from_secs(180);
/// A launcher that refuses exits within the lock wait and the probe timeout.
const EXIT_WITHIN: Duration = Duration::from_secs(90);
/// Subordinate ranges far from any host's own (`/etc/subuid` starts at 100000).
const RANGE_BASE: u32 = 1_000_000_000;
const RANGE_COUNT: u32 = 65_536;

static SERIAL: Mutex<()> = Mutex::new(());

fn must<T, E: Display>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|error| panic!("{what}: {error}"))
}

fn note(message: &str) {
    let _ = writeln!(io::stderr(), "integration: {message}");
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// One test's hold on the node: the lock that keeps the tests one at a time, and the volumes that
/// existed before it, which [`Session::finish`] compares against.
struct Session {
    _serial: MutexGuard<'static, ()>,
    volumes: BTreeSet<String>,
}

impl Session {
    /// Asserts that the test left no volume behind; call it after dropping the test's cases.
    fn finish(self) {
        let leaked: Vec<String> = volumes().difference(&self.volumes).cloned().collect();
        assert!(leaked.is_empty(), "volumes left behind: {leaked:?}");
    }
}

/// Whether to run, holding the node for the test. `None` skips, outside CI.
fn begin(test: &str) -> Option<Session> {
    let wanted = std::env::var("SRD_IT").is_ok_and(|value| value == "1");
    let required = std::env::var("SRD_IT_REQUIRE").is_ok_and(|value| value == "1");
    if !wanted {
        assert!(!required, "{test}: SRD_IT_REQUIRE=1 but SRD_IT is not 1");
        note(&format!("{test} skipped: set SRD_IT=1 to run it"));
        return None;
    }
    if let Ok(restricted) = std::fs::read_to_string(APPARMOR_USERNS) {
        assert!(
            restricted.trim() == "0",
            "{APPARMOR_USERNS} is {}: rootlesskit cannot start; set it to 0 for the run",
            restricted.trim()
        );
    }
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let image = image();
    assert!(
        docker(&["image", "inspect", &image]).status.success(),
        "{image} is not built: make image-build"
    );
    Some(Session {
        _serial: guard,
        volumes: volumes(),
    })
}

fn image() -> String {
    env_or("SRD_IT_IMAGE", DEFAULT_IMAGE)
}

/// `SRD_IT_ROOT`, refused unless it is an absolute path of at least two plain components: the
/// tests create and delete directories in it as root.
fn root() -> String {
    let root = env_or("SRD_IT_ROOT", DEFAULT_ROOT);
    assert!(
        valid_root(&root),
        "SRD_IT_ROOT {root:?} must be an absolute path of two or more components, each of letters, \
         digits, '.', '_' or '-' (not . or ..)"
    );
    root
}

fn valid_root(root: &str) -> bool {
    let Some(relative) = root.strip_prefix('/') else {
        return false;
    };
    let components: Vec<&str> = relative.split('/').collect();
    components.len() >= 2 && components.iter().all(|component| plain_name(component))
}

/// A path component with nothing a shell or a path could read twice: letters, digits, `.`, `_`
/// and `-`, and not `.` or `..`.
fn plain_name(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `SRD_IT_ROOT`'s parent, which the helper mounts, and its last component.
fn root_parts() -> (String, String) {
    let root = root();
    let (parent, base) = root.rsplit_once('/').unwrap_or(("", ""));
    (parent.to_owned(), base.to_owned())
}

fn docker(args: &[&str]) -> Output {
    must(
        Command::new("docker").args(args).output(),
        "run the docker CLI",
    )
}

fn docker_ok(args: &[&str]) -> String {
    let output = docker(args);
    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn docker_with_input(args: &[&str], input: &[u8]) -> String {
    let mut child = must(
        Command::new("docker")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn(),
        "run the docker CLI",
    );
    let mut stdin = child.stdin.take().unwrap_or_else(|| panic!("no stdin"));
    must(stdin.write_all(input), "write to docker");
    drop(stdin);
    let output = must(child.wait_with_output(), "wait for docker");
    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Runs `script` as root with `SRD_IT_ROOT`'s parent mounted at `/parent`. Paths go in `args`,
/// the script's `"$1"`, `"$2"`, …, never into the script text.
fn host_root_command(script: &str, args: &[&str]) -> Output {
    let (parent, _) = root_parts();
    let mount = format!("{parent}:/parent");
    let image = image();
    let mut all = vec![
        "run",
        "--rm",
        "--user",
        "0:0",
        "--network",
        "none",
        "-v",
        &mount,
        "--entrypoint",
        "/bin/sh",
        &image,
        "-c",
        script,
        "sh",
    ];
    all.extend_from_slice(args);
    docker(&all)
}

/// [`host_root_command`], which must succeed: its stdout.
fn as_host_root(script: &str, args: &[&str]) -> String {
    let output = host_root_command(script, args);
    assert!(
        output.status.success(),
        "as root: {script} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The path under `/parent` that the helper sees for a host path under `SRD_IT_ROOT`.
fn in_helper(path: &str) -> String {
    let root = root();
    let (_, base) = root_parts();
    let rest = path
        .strip_prefix(&root)
        .unwrap_or_else(|| panic!("{path} is not under {root}"));
    format!("/parent/{base}{rest}")
}

fn socket_gid() -> String {
    must(
        std::fs::metadata(HOST_SOCKET).map(|meta| {
            use std::os::unix::fs::MetadataExt as _;
            meta.gid().to_string()
        }),
        "stat the host socket",
    )
}

fn volumes() -> BTreeSet<String> {
    docker_ok(&["volume", "ls", "-q"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn inspect(name: &str) -> Option<Value> {
    let output = docker(&["inspect", "--type", "container", name]);
    if !output.status.success() {
        return None;
    }
    let parsed: Value = must(serde_json::from_slice(&output.stdout), "parse inspect");
    parsed.get(0).cloned()
}

fn exists(name: &str) -> bool {
    inspect(name).is_some()
}

fn logs(name: &str) -> String {
    let output = docker(&["logs", name]);
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The launch flags of one instance, each replaceable.
#[derive(Clone)]
struct Flags {
    instance: String,
    subuid: String,
    data_root: String,
    socket_dir: String,
    mounts: Vec<String>,
}

impl Flags {
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "launch".to_owned(),
            "--instance".to_owned(),
            self.instance.clone(),
            "--subuid".to_owned(),
            self.subuid.clone(),
            "--data-root".to_owned(),
            self.data_root.clone(),
            "--socket-dir".to_owned(),
            self.socket_dir.clone(),
            "--network".to_owned(),
            NETWORK.to_owned(),
            "--memory".to_owned(),
            "2g".to_owned(),
            "--pids-limit".to_owned(),
            "4096".to_owned(),
            "--log-level".to_owned(),
            "debug".to_owned(),
        ];
        for mount in &self.mounts {
            args.extend(["--mount".to_owned(), mount.clone()]);
        }
        args
    }
}

/// One instance's host directories and the containers the test made, all removed on drop.
struct Case {
    name: String,
    range_start: u32,
    data_root: String,
    socket_dir: String,
    work: String,
    containers: Vec<String>,
}

impl Case {
    /// The instance `it-<name>`, its range at slot `slot`, and its three directories, owned by the
    /// daemon UID, mode 0750, under the root-owned `SRD_IT_ROOT`.
    fn new(name: &str, slot: u32) -> Self {
        assert!(
            plain_name(name) && !name.contains('.'),
            "case name {name:?} is not plain"
        );
        let root = root();
        let (_, base) = root_parts();
        as_host_root(
            r#"install -d -o 0 -g 0 -m 0755 "$1" && rm -rf -- "$1/$2"-*"#,
            &[&format!("/parent/{base}"), name],
        );
        let case = Self {
            name: name.to_owned(),
            range_start: RANGE_BASE + slot * RANGE_COUNT,
            data_root: format!("{root}/{name}-data"),
            socket_dir: format!("{root}/{name}-socket"),
            work: format!("{root}/{name}-work"),
            containers: Vec::new(),
        };
        let leaves: Vec<String> = [&case.data_root, &case.socket_dir, &case.work]
            .iter()
            .map(|path| in_helper(path))
            .collect();
        let mut args = vec![UID_TEXT];
        args.extend(leaves.iter().map(String::as_str));
        as_host_root(
            r#"owner="$1"; shift; install -d -o "$owner" -g "$owner" -m 0750 "$@""#,
            &args,
        );
        let _ = docker(&["network", "create", NETWORK]);
        case
    }

    fn flags(&self) -> Flags {
        Flags {
            instance: format!("it-{}", self.name),
            subuid: format!("{}:{RANGE_COUNT}", self.range_start),
            data_root: self.data_root.clone(),
            socket_dir: self.socket_dir.clone(),
            mounts: vec![format!("{}:{}", self.work, self.work)],
        }
    }

    fn daemon(flags: &Flags) -> String {
        format!("swarm-rootless-docker-daemon-{}", flags.instance)
    }

    /// Starts a launcher as a Swarm task would run it: non-root in the socket's group, no
    /// capability, no network, a read-only root with tmpfs where it writes.
    fn start(&mut self, label: &str, flags: &Flags) -> String {
        let name = format!("srd-it-{}-{label}", self.name);
        let _ = docker(&["rm", "-fv", &name]);
        self.containers.push(name.clone());
        let user = format!("65534:{}", socket_gid());
        let socket = format!("{HOST_SOCKET}:{HOST_SOCKET}");
        let image = image();
        let mut args: Vec<String> = [
            "run",
            "-d",
            "--name",
            &name,
            "--user",
            &user,
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/home/rootless/.local/share/docker",
            "--tmpfs",
            "/var/lib/docker",
            "--network",
            "none",
            "-v",
            &socket,
            &image,
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
        args.extend(flags.args());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        docker_ok(&args);
        name
    }

    /// Waits for the launcher to be healthy; panics with its logs if it exits or takes too long.
    fn wait_healthy(launcher: &str) {
        let started = Instant::now();
        while started.elapsed() < HEALTHY_WITHIN {
            let state = inspect(launcher).unwrap_or_else(|| panic!("{launcher} is gone"));
            assert!(
                state["State"]["Running"] == Value::Bool(true),
                "{launcher} exited:\n{}",
                logs(launcher)
            );
            if state["State"]["Health"]["Status"] == "healthy" {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
        panic!(
            "{launcher} not healthy within {HEALTHY_WITHIN:?}:\n{}",
            logs(launcher)
        );
    }

    /// Waits for the launcher to exit: its code and logs.
    fn wait_exit(launcher: &str, within: Duration) -> (i64, String) {
        let started = Instant::now();
        while started.elapsed() < within {
            let state = inspect(launcher).unwrap_or_else(|| panic!("{launcher} is gone"));
            if state["State"]["Running"] == Value::Bool(false) {
                let code = state["State"]["ExitCode"].as_i64().unwrap_or(-1);
                return (code, logs(launcher));
            }
            thread::sleep(Duration::from_millis(500));
        }
        panic!(
            "{launcher} still runs after {within:?}:\n{}",
            logs(launcher)
        );
    }

    /// Runs a launcher that must refuse: exit 1, and its logs.
    fn refused(&mut self, label: &str, flags: &Flags) -> String {
        let launcher = self.start(label, flags);
        let (code, logs) = Self::wait_exit(&launcher, EXIT_WITHIN);
        assert_eq!(code, 1, "{launcher} exited {code}, want 1:\n{logs}");
        let label = format!(
            "label=io.github.leinardi.swarm-rootless-docker.instance={}",
            flags.instance
        );
        let left = docker_ok(&["ps", "-aq", "--filter", &label]);
        assert!(left.is_empty(), "{launcher} refused but left daemon {left}");
        logs
    }

    /// The `docker` CLI as the consumer: the daemon UID with the socket directory read-only.
    fn consumer(&self, args: &[&str]) -> String {
        let socket = format!("{}:/run/srd-it:ro", self.socket_dir);
        let image = image();
        let mut all = vec![
            "run",
            "--rm",
            "-i",
            "--user",
            "568:568",
            "--network",
            "none",
            "-v",
            &socket,
            "--entrypoint",
            "docker",
            &image,
            "-H",
            "unix:///run/srd-it/docker.sock",
        ];
        all.extend_from_slice(args);
        docker_ok(&all)
    }

    fn mkdir_root_owned(path: &str, mode: &str) {
        as_host_root(
            r#"install -d -o 0 -g 0 -m "$1" "$2""#,
            &[mode, &in_helper(path)],
        );
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        // -v: a container from the image carries upstream's two anonymous volumes.
        for container in &self.containers {
            let _ = docker(&["rm", "-fv", container]);
        }
        let _ = docker(&["rm", "-fv", &Self::daemon(&self.flags())]);
        let (_, base) = root_parts();
        let _ = host_root_command(
            r#"rm -rf -- "$1/$2"-*"#,
            &[&format!("/parent/{base}"), &self.name],
        );
    }
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

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
    docker_ok(&["pull", "-q", INNER_IMAGE]);
    let image_tar = must(
        Command::new("docker").args(["save", INNER_IMAGE]).output(),
        "save the inner image",
    );
    assert!(image_tar.status.success(), "docker save {INNER_IMAGE}");
    let socket = format!("{}:/run/srd-it:ro", case.socket_dir);
    let our_image = image();
    docker_with_input(
        &[
            "run",
            "--rm",
            "-i",
            "--user",
            "568:568",
            "--network",
            "none",
            "-v",
            &socket,
            "--entrypoint",
            "docker",
            &our_image,
            "-H",
            "unix:///run/srd-it/docker.sock",
            "load",
            "-q",
        ],
        &image_tar.stdout,
    );
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

/// The scratch root the tests delete in as root must be plain: no space, no `..`, not `/` or a
/// top-level directory. Runs without `SRD_IT`.
#[test]
fn only_a_plain_scratch_root_is_accepted() {
    for accepted in ["/srv/srd-it", "/var/tmp/srd_it.1", "/a/b/c"] {
        assert!(valid_root(accepted), "{accepted:?} refused");
    }
    for refused in [
        "/srv/srd it",
        "/srv",
        "/",
        "",
        "srv/srd-it",
        "/srv/../etc",
        "/srv/./srd-it",
        "/srv//srd-it",
        "/srv/srd-it/",
        "/srv/srd-it*",
        "/srv/$(id)",
        "/srv/srd;rm",
    ] {
        assert!(!valid_root(refused), "{refused:?} accepted");
    }
}
