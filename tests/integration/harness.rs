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

//! The harness: the docker CLI, the root helper, the scratch root, one instance's directories and
//! launchers, and the per-test session.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::{self, Write as _};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use swarm_rootless_docker::config::{
    ExtraMount, HostPath, IdRange, InstanceName, LaunchConfig, LaunchValues, MemoryBytes,
    NetworkAlias, NetworkName, PidsLimit,
};

pub(crate) const DEFAULT_IMAGE: &str = "ghcr.io/leinardi/swarm-rootless-docker:dev-uid568";
pub(crate) const DEFAULT_ROOT: &str = "/srv/srd-it";
pub(crate) const NETWORK: &str = "srd-it";
pub(crate) const UID_TEXT: &str = "568";
pub(crate) const HOST_SOCKET: &str = "/var/run/docker.sock";
pub(crate) const INNER_IMAGE: &str = "busybox:1.37";
pub(crate) const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
/// A launcher must be healthy within the image's healthcheck start period.
pub(crate) const HEALTHY_WITHIN: Duration = Duration::from_secs(180);
/// A launcher that refuses exits within the lock wait and the probe timeout.
pub(crate) const EXIT_WITHIN: Duration = Duration::from_secs(90);
/// Subordinate ranges far from any host's own (`/etc/subuid` starts at 100000).
pub(crate) const RANGE_BASE: u32 = 1_000_000_000;
pub(crate) const RANGE_COUNT: u32 = 65_536;

pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

pub(crate) fn must<T, E: Display>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|error| panic!("{what}: {error}"))
}

pub(crate) fn note(message: &str) {
    let _ = writeln!(io::stderr(), "integration: {message}");
}

pub(crate) fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// One test's hold on the node: the lock that keeps the tests one at a time, and the volumes that
/// existed before it, which [`Session::finish`] compares against.
pub(crate) struct Session {
    pub(crate) _serial: MutexGuard<'static, ()>,
    pub(crate) volumes: BTreeSet<String>,
}

impl Session {
    /// Asserts that the test left no volume behind; call it after dropping the test's cases.
    pub(crate) fn finish(self) {
        let leaked: Vec<String> = volumes().difference(&self.volumes).cloned().collect();
        assert!(leaked.is_empty(), "volumes left behind: {leaked:?}");
    }
}

/// Whether to run, holding the node for the test. `None` skips, outside CI.
pub(crate) fn begin(test: &str) -> Option<Session> {
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

pub(crate) fn image() -> String {
    env_or("SRD_IT_IMAGE", DEFAULT_IMAGE)
}

/// `SRD_IT_ROOT`, refused unless it is an absolute path of at least two plain components: the
/// tests create and delete directories in it as root.
pub(crate) fn root() -> String {
    let root = env_or("SRD_IT_ROOT", DEFAULT_ROOT);
    assert!(
        valid_root(&root),
        "SRD_IT_ROOT {root:?} must be an absolute path of two or more components, each of letters, \
         digits, '.', '_' or '-' (not . or ..)"
    );
    root
}

pub(crate) fn valid_root(root: &str) -> bool {
    let Some(relative) = root.strip_prefix('/') else {
        return false;
    };
    let components: Vec<&str> = relative.split('/').collect();
    components.len() >= 2 && components.iter().all(|component| plain_name(component))
}

/// A path component with nothing a shell or a path could read twice: letters, digits, `.`, `_`
/// and `-`, and not `.` or `..`.
pub(crate) fn plain_name(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `SRD_IT_ROOT`'s parent, which the helper mounts, and its last component.
pub(crate) fn root_parts() -> (String, String) {
    let root = root();
    let (parent, base) = root.rsplit_once('/').unwrap_or(("", ""));
    (parent.to_owned(), base.to_owned())
}

pub(crate) fn docker(args: &[&str]) -> Output {
    must(
        Command::new("docker").args(args).output(),
        "run the docker CLI",
    )
}

pub(crate) fn docker_ok(args: &[&str]) -> String {
    let output = docker(args);
    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub(crate) fn docker_with_input(args: &[&str], input: &[u8]) -> String {
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
pub(crate) fn host_root_command(script: &str, args: &[&str]) -> Output {
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
pub(crate) fn as_host_root(script: &str, args: &[&str]) -> String {
    let output = host_root_command(script, args);
    assert!(
        output.status.success(),
        "as root: {script} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The path under `/parent` that the helper sees for a host path under `SRD_IT_ROOT`.
pub(crate) fn in_helper(path: &str) -> String {
    let root = root();
    let (_, base) = root_parts();
    let rest = path
        .strip_prefix(&root)
        .unwrap_or_else(|| panic!("{path} is not under {root}"));
    format!("/parent/{base}{rest}")
}

pub(crate) fn socket_gid() -> String {
    must(
        std::fs::metadata(HOST_SOCKET).map(|meta| {
            use std::os::unix::fs::MetadataExt as _;
            meta.gid().to_string()
        }),
        "stat the host socket",
    )
}

pub(crate) fn volumes() -> BTreeSet<String> {
    docker_ok(&["volume", "ls", "-q"])
        .lines()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn inspect(name: &str) -> Option<Value> {
    let output = docker(&["inspect", "--type", "container", name]);
    if !output.status.success() {
        return None;
    }
    let parsed: Value = must(serde_json::from_slice(&output.stdout), "parse inspect");
    parsed.get(0).cloned()
}

pub(crate) fn exists(name: &str) -> bool {
    inspect(name).is_some()
}

pub(crate) fn logs(name: &str) -> String {
    let output = docker(&["logs", name]);
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The launch flags of one instance, each replaceable.
#[derive(Clone)]
pub(crate) struct Flags {
    pub(crate) instance: String,
    pub(crate) subuid: String,
    pub(crate) data_root: String,
    pub(crate) socket_dir: String,
    pub(crate) mounts: Vec<String>,
    pub(crate) aliases: Vec<String>,
    pub(crate) memory: String,
}

impl Flags {
    pub(crate) fn args(&self) -> Vec<String> {
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
            self.memory.clone(),
            "--pids-limit".to_owned(),
            "4096".to_owned(),
            "--log-level".to_owned(),
            "debug".to_owned(),
        ];
        for mount in &self.mounts {
            args.extend(["--mount".to_owned(), mount.clone()]);
        }
        for alias in &self.aliases {
            args.extend(["--alias".to_owned(), alias.clone()]);
        }
        args
    }

    /// The configuration the launcher builds from these flags, for labels written exactly as it
    /// writes them.
    pub(crate) fn config(&self) -> LaunchConfig {
        let mut mounts = Vec::new();
        for mount in &self.mounts {
            mounts.push(must(ExtraMount::parse(mount), "--mount"));
        }
        let mut aliases = Vec::new();
        for alias in &self.aliases {
            aliases.push(must(NetworkAlias::parse(alias), "--alias"));
        }
        must(
            LaunchConfig::new(LaunchValues {
                instance: must(InstanceName::parse(&self.instance), "--instance"),
                subuid: must(IdRange::parse(&self.subuid), "--subuid"),
                subgid: None,
                data_root: must(HostPath::parse(&self.data_root), "--data-root"),
                socket_dir: must(HostPath::parse(&self.socket_dir), "--socket-dir"),
                mounts,
                network: must(NetworkName::parse(NETWORK), "--network"),
                aliases,
                memory: must(MemoryBytes::parse(&self.memory), "--memory"),
                pids_limit: must(PidsLimit::parse("4096"), "--pids-limit"),
                cpus: None,
            }),
            "the launch configuration",
        )
    }
}

/// One instance's host directories and the containers the test made, all removed on drop.
pub(crate) struct Case {
    pub(crate) name: String,
    pub(crate) range_start: u32,
    pub(crate) data_root: String,
    pub(crate) socket_dir: String,
    pub(crate) work: String,
    pub(crate) containers: Vec<String>,
}

impl Case {
    /// The instance `it-<name>`, its range at slot `slot`, and its three directories, owned by the
    /// daemon UID, mode 0750, under the root-owned `SRD_IT_ROOT`.
    pub(crate) fn new(name: &str, slot: u32) -> Self {
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

    pub(crate) fn flags(&self) -> Flags {
        Flags {
            instance: format!("it-{}", self.name),
            subuid: format!("{}:{RANGE_COUNT}", self.range_start),
            data_root: self.data_root.clone(),
            socket_dir: self.socket_dir.clone(),
            mounts: vec![format!("{}:{}", self.work, self.work)],
            aliases: Vec::new(),
            memory: "2g".to_owned(),
        }
    }

    pub(crate) fn daemon(flags: &Flags) -> String {
        format!("swarm-rootless-docker-daemon-{}", flags.instance)
    }

    /// Starts a launcher as a Swarm task would run it: non-root in the socket's group, no
    /// capability, no network, a read-only root with tmpfs where it writes.
    pub(crate) fn start(&mut self, label: &str, flags: &Flags) -> String {
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
    pub(crate) fn wait_healthy(launcher: &str) {
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
    pub(crate) fn wait_exit(launcher: &str, within: Duration) -> (i64, String) {
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
    pub(crate) fn refused(&mut self, label: &str, flags: &Flags) -> String {
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

    /// [`Case::consumer`] as a named, detached container, given up after `within`: its exit code
    /// and stdout, or `None` when it did not finish in time (it is then removed). For a daemon
    /// that may accept a connection and never answer.
    pub(crate) fn consumer_within(
        &mut self,
        label: &str,
        args: &[&str],
        within: Duration,
    ) -> Option<(i64, String)> {
        let name = self.track(&format!("srd-it-{}-{label}", self.name));
        let socket = format!("{}:/run/srd-it:ro", self.socket_dir);
        let image = image();
        let mut all = vec![
            "run",
            "-d",
            "--name",
            &name,
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
        docker_ok(&all);
        let started = Instant::now();
        let finished = loop {
            let state = inspect(&name).unwrap_or_else(|| panic!("{name} is gone"));
            if state["State"]["Running"] == Value::Bool(false) {
                break Some(state["State"]["ExitCode"].as_i64().unwrap_or(-1));
            }
            if started.elapsed() >= within {
                break None;
            }
            thread::sleep(Duration::from_millis(250));
        };
        let answer = finished.map(|code| {
            let output = docker(&["logs", &name]);
            (
                code,
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            )
        });
        let _ = docker(&["rm", "-fv", &name]);
        answer
    }

    /// The `docker` CLI as the consumer: the daemon UID with the socket directory read-only.
    pub(crate) fn consumer(&self, args: &[&str]) -> String {
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

    pub(crate) fn mkdir_root_owned(path: &str, mode: &str) {
        as_host_root(
            r#"install -d -o 0 -g 0 -m "$1" "$2""#,
            &[mode, &in_helper(path)],
        );
    }
}

impl Case {
    /// Registers a container the test makes itself, so the drop removes it (with its volumes).
    pub(crate) fn track(&mut self, name: &str) -> String {
        let _ = docker(&["rm", "-fv", name]);
        self.containers.push(name.to_owned());
        name.to_owned()
    }

    /// Loads [`INNER_IMAGE`] into this instance's daemon, from the host, as the consumer.
    pub(crate) fn load_inner_image(&self) {
        docker_ok(&["pull", "-q", INNER_IMAGE]);
        let image_tar = must(
            Command::new("docker").args(["save", INNER_IMAGE]).output(),
            "save the inner image",
        );
        assert!(image_tar.status.success(), "docker save {INNER_IMAGE}");
        let socket = format!("{}:/run/srd-it:ro", self.socket_dir);
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
    }
}

/// How a launcher ended up, for launchers racing each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Healthy,
    Exited(i64),
}

/// Waits until the launcher is healthy or has exited.
pub(crate) fn outcome(launcher: &str) -> Outcome {
    let started = Instant::now();
    while started.elapsed() < HEALTHY_WITHIN {
        let state = inspect(launcher).unwrap_or_else(|| panic!("{launcher} is gone"));
        if state["State"]["Running"] == Value::Bool(false) {
            return Outcome::Exited(state["State"]["ExitCode"].as_i64().unwrap_or(-1));
        }
        if state["State"]["Health"]["Status"] == "healthy" {
            return Outcome::Healthy;
        }
        thread::sleep(Duration::from_secs(1));
    }
    panic!("{launcher} neither healthy nor exited:\n{}", logs(launcher));
}

/// The first 12 hex digits of a container's ID, as the launcher names containers.
pub(crate) fn short_id(name: &str) -> String {
    inspect(name).unwrap_or_else(|| panic!("no container {name}"))["Id"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(12)
        .collect()
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

pub(crate) fn contains(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
