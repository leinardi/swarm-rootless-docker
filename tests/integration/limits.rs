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

//! The daemon's memory limit: the one limit its workloads share, since there are no cgroups inside.

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::harness::*;

/// How long the daemon, or its launcher's exit, may take to show which way the OOM kill went.
const RECOVERY: Duration = Duration::from_secs(120);

/// The longest the hog may run before it is given up.
const HOG_WITHIN: Duration = Duration::from_secs(120);
/// The longest one probe of the daemon may take.
const PROBE_WITHIN: Duration = Duration::from_secs(15);

/// Whether the daemon answers the consumer now: a fresh API round trip, not a cached status,
/// given up (and its container removed) when `within` runs out, since a daemon may accept a
/// connection and never answer.
fn answers(case: &mut Case, attempt: &mut u32, within: Duration) -> bool {
    *attempt += 1;
    let label = format!("info-{attempt}");
    let within = within.min(PROBE_WITHIN);
    matches!(
        case.consumer_within(&label, &["info", "--format", "{{.ID}}"], within),
        Some((0, id)) if !id.is_empty()
    )
}

/// A memory hog in a consumer's container stays inside the daemon container's limit: the OOM
/// killer acts there, the host and a sibling container are untouched. Whether dockerd survives is
/// the kernel's choice, so both outcomes pass: the launcher stays healthy, or it exits non-zero
/// and a restarted launcher brings a healthy daemon back.
#[test]
fn a_memory_hog_stays_inside_the_daemons_limit() {
    let Some(session) = begin("a_memory_hog_stays_inside_the_daemons_limit") else {
        return;
    };
    let mut case = Case::new("memory", 19);
    let flags = Flags {
        memory: "512m".to_owned(),
        ..case.flags()
    };
    let launcher = case.start("launcher", &flags);
    Case::wait_healthy(&launcher);
    let daemon = Case::daemon(&flags);
    case.load_inner_image();
    let image = image();
    let sibling = case.track("srd-it-memory-sibling");
    docker_ok(&[
        "run",
        "-d",
        "--name",
        &sibling,
        "--network",
        "none",
        "--entrypoint",
        "sleep",
        &image,
        "infinity",
    ]);

    let since = must(SystemTime::now().duration_since(UNIX_EPOCH), "the clock")
        .as_secs()
        .to_string();
    // Its result is the OOM kill's, a lost connection if dockerd went too, or nothing at all if
    // the daemon wedged: not checked, only bounded.
    let _ = case.consumer_within(
        "hog",
        &[
            "run",
            "--rm",
            "--network",
            "none",
            INNER_IMAGE,
            "sh",
            "-c",
            "tail /dev/zero",
        ],
        HOG_WITHIN,
    );
    let until = must(SystemTime::now().duration_since(UNIX_EPOCH), "the clock")
        .as_secs()
        .saturating_add(1)
        .to_string();
    let filter = format!("container={daemon}");
    let ooms = docker_ok(&[
        "events",
        "--since",
        &since,
        "--until",
        &until,
        "--filter",
        &filter,
        "--filter",
        "event=oom",
        "--format",
        "{{.Action}}",
    ]);
    assert!(
        contains(&ooms, "oom"),
        "no OOM recorded on the daemon container"
    );

    let neighbour = inspect(&sibling).unwrap_or_else(|| panic!("the sibling is gone"));
    assert_eq!(neighbour["State"]["Running"], Value::Bool(true));
    assert_eq!(neighbour["State"]["OOMKilled"], Value::Bool(false));

    // Docker's health status may still be the one from before the kill: only a fresh answer from
    // the daemon, or the launcher's exit, says which way it went.
    let deadline = Instant::now() + RECOVERY;
    let mut attempt = 0;
    loop {
        assert!(
            Instant::now() < deadline,
            "within {RECOVERY:?} the daemon neither answered nor did its launcher exit:\n{}",
            logs(&launcher)
        );
        let state = inspect(&launcher).unwrap_or_else(|| panic!("{launcher} is gone"));
        if state["State"]["Running"] == Value::Bool(false) {
            let code = state["State"]["ExitCode"].as_i64().unwrap_or(-1);
            assert_ne!(code, 0, "the launcher exited 0 after its daemon was killed");
            note(&format!("the launcher exited {code}; restarting it"));
            let restarted = case.start("restarted", &flags);
            Case::wait_healthy(&restarted);
            assert!(
                answers(&mut case, &mut attempt, PROBE_WITHIN),
                "the restarted daemon does not answer"
            );
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if answers(&mut case, &mut attempt, left) {
            note("the daemon survived the OOM kill and answers");
            break;
        }
        thread::sleep(Duration::from_secs(2));
    }
    drop(case);
    session.finish();
}
