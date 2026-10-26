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

//! The launcher as a Swarm service, as the homelab runs it. Off unless `SRD_IT_SWARM=1`: it
//! deploys into the swarm the node is in, so it is for a throwaway node (CI initialises one).

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::*;

const SERVICE: &str = "srd-it-service";
const SERVICE_NETWORK: &str = "srd-it-swarm";
const OTHER_TAG: &str = "srd-it-other:latest";
const SERVICE_LABEL: &str = "com.docker.swarm.service.name=srd-it-service";

/// The service's running task container, once healthy.
fn healthy_task() -> String {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(240) {
        let filter = format!("label={SERVICE_LABEL}");
        let tasks = docker_ok(&[
            "ps",
            "-q",
            "--filter",
            &filter,
            "--filter",
            "health=healthy",
        ]);
        if let Some(task) = tasks.lines().next() {
            return task.to_owned();
        }
        thread::sleep(Duration::from_secs(2));
    }
    panic!(
        "no healthy task of {SERVICE}:\n{}",
        docker_ok(&["service", "ps", "--no-trunc", SERVICE])
    );
}

/// Removes the service on every path, failures included, and waits for its tasks to stop before
/// the case's storage goes: Swarm would otherwise keep replacing launchers against deleted paths.
/// Declared after the [`Case`], so it drops first.
struct Service;

impl Drop for Service {
    fn drop(&mut self) {
        let _ = docker(&["service", "rm", SERVICE]);
        let filter = format!("label={SERVICE_LABEL}");
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(90) {
            let tasks = docker(&["ps", "-aq", "--filter", &filter]);
            if tasks.status.success() && tasks.stdout.iter().all(u8::is_ascii_whitespace) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        let _ = docker(&["network", "rm", SERVICE_NETWORK]);
        let _ = docker(&["image", "rm", OTHER_TAG]);
    }
}

/// Waits until `check` holds, or panics naming `what`.
fn eventually(what: &str, within: Duration, check: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < within {
        if check() {
            return;
        }
        thread::sleep(Duration::from_secs(2));
    }
    panic!("not within {within:?}: {what}");
}

/// Creates the service as the homelab's compose file would: non-root in the socket's group, no
/// capability, read-only with tmpfs where it writes (upstream's two volumes included), the socket
/// mounted, a 40 s grace period and stop-first updates.
fn create_service(flags: &Flags, image: &str) {
    let user = format!("65534:{}", socket_gid());
    let socket = format!("type=bind,source={HOST_SOCKET},target={HOST_SOCKET}");
    let mut args: Vec<String> = [
        "service",
        "create",
        "--detach",
        "--quiet",
        "--no-resolve-image",
        "--name",
        SERVICE,
        "--network",
        SERVICE_NETWORK,
        "--user",
        &user,
        "--cap-drop",
        "ALL",
        "--read-only",
        "--mount",
        "type=tmpfs,target=/tmp",
        "--mount",
        "type=tmpfs,target=/home/rootless/.local/share/docker",
        "--mount",
        "type=tmpfs,target=/var/lib/docker",
        "--mount",
        &socket,
        "--stop-grace-period",
        "40s",
        "--update-order",
        "stop-first",
        image,
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    args.extend(flags.args());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    docker_ok(&args);
}

/// The service runs a healthy launcher on an internal overlay network, Swarm's logs carry
/// dockerd's, an image update swaps the daemon to the new image (stop-first), and removing the
/// service removes the daemon.
#[test]
fn the_launcher_runs_as_a_swarm_service() {
    if !std::env::var("SRD_IT_SWARM").is_ok_and(|value| value == "1") {
        note(
            "the_launcher_runs_as_a_swarm_service skipped: set SRD_IT_SWARM=1 on a throwaway node",
        );
        return;
    }
    let Some(session) = begin("the_launcher_runs_as_a_swarm_service") else {
        return;
    };
    assert_eq!(
        docker_ok(&["info", "--format", "{{.Swarm.LocalNodeState}}"]),
        "active",
        "SRD_IT_SWARM=1 needs a swarm (docker swarm init)"
    );
    let case = Case::new("swarm", 20);
    let flags = case.flags();
    let daemon = Case::daemon(&flags);
    let image = image();
    let _ = docker(&["service", "rm", SERVICE]);
    let service = Service;
    let _ = docker(&[
        "network",
        "create",
        "--driver",
        "overlay",
        "--internal",
        SERVICE_NETWORK,
    ]);
    create_service(&flags, &image);

    let task = healthy_task();
    let first_image = inspect(&daemon).unwrap_or_else(|| panic!("no daemon"))["Image"].clone();
    assert_eq!(
        first_image,
        inspect(&task).unwrap_or_else(|| panic!("no task"))["Image"]
    );
    eventually(
        "dockerd's lines in the service logs",
        Duration::from_secs(30),
        || {
            let output = docker(&["service", "logs", "--raw", SERVICE]);
            contains(&String::from_utf8_lossy(&output.stdout), "API listen on")
                || contains(&String::from_utf8_lossy(&output.stderr), "API listen on")
        },
    );

    // Another image: the same, with one more label, so its ID differs.
    docker_with_input(
        &["build", "-q", "-t", OTHER_TAG, "-"],
        format!("FROM {image}\nLABEL io.github.leinardi.srd-it.variant=other\n").as_bytes(),
    );
    docker_ok(&[
        "service",
        "update",
        "--detach",
        "--quiet",
        "--no-resolve-image",
        "--image",
        OTHER_TAG,
        SERVICE,
    ]);
    let other_id = docker_ok(&["image", "inspect", "--format", "{{.Id}}", OTHER_TAG]);
    eventually(
        "the daemon on the new image",
        Duration::from_secs(240),
        || inspect(&daemon).is_some_and(|state| state["Image"] == Value::String(other_id.clone())),
    );
    healthy_task();

    docker_ok(&["service", "rm", SERVICE]);
    eventually(
        "the daemon removed with the service",
        Duration::from_secs(60),
        || !exists(&daemon),
    );
    drop(service);
    drop(case);
    session.finish();
}
