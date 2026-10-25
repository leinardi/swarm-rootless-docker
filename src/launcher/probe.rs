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

//! Running the host-side probe ([`crate::probe`]) in its container: create, attach, start, read
//! its report to the end of its output, then its exit code; all within one timeout, and the
//! container removed by ID whatever happened.

use std::time::Duration;

use super::{LaunchError, docker_error, remove};
use crate::config::{AbsolutePath, LaunchConfig};
use crate::docker::{DockerApi, Output};
use crate::node::short;
use crate::probe::ProbeReport;
use crate::spec::{self, ContainerId, ImageIdentity, ProbeRequest, StorageIds};

/// The most output the probe may print; its report is one line of a few kilobytes.
const MAX_OUTPUT: usize = 64 * 1024;
/// The probe's exit codes, with the report it prints.
const EXIT_ACCEPTED: i64 = 0;
const EXIT_REFUSED: i64 = 1;

/// Probes `config`'s host paths and ranges, `denied` paths excluded, and returns where each path
/// lies on the host.
///
/// # Errors
///
/// [`LaunchError::ProbeRefused`] with the probe's reason; [`LaunchError::Probe`] when it did not
/// report in time, or its report and exit code do not agree; a Docker error otherwise.
pub(super) async fn run(
    docker: &impl DockerApi,
    config: &LaunchConfig,
    image: &ImageIdentity,
    launcher: &ContainerId,
    denied: &[AbsolutePath],
    timeout: Duration,
) -> Result<StorageIds, LaunchError> {
    let request = ProbeRequest::new(config, image, denied);
    let spec = spec::probe_spec(config, image, launcher, &request)?;
    let probe = docker
        .create_container(&spec)
        .await
        .map_err(|source| docker_error("create the probe container", source))?;
    let finished = tokio::time::timeout(timeout, report(docker, &probe)).await;
    let removed = remove(docker, &probe).await;
    let (code, stdout) = finished
        .map_err(|_elapsed| LaunchError::Probe(format!("no report within {timeout:?}")))??;
    removed?;
    let stdout = String::from_utf8_lossy(&stdout);
    let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        return Err(LaunchError::Probe(format!(
            "it printed more than its one report line: {stdout:?}"
        )));
    }
    let parsed = serde_json::from_str::<ProbeReport>(line);
    match (code, parsed) {
        (
            EXIT_ACCEPTED,
            Ok(ProbeReport::Accepted {
                data_root,
                socket_dir,
                mounts,
            }),
        ) => Ok(StorageIds::new(config, data_root, socket_dir, mounts)?),
        (EXIT_REFUSED, Ok(ProbeReport::Refused { reason })) => {
            Err(LaunchError::ProbeRefused(reason))
        }
        (code, Ok(report)) => Err(LaunchError::Probe(format!(
            "exit code {code} with the report {report:?}"
        ))),
        (code, Err(error)) => Err(LaunchError::Probe(format!(
            "exit code {code}, and its report {line:?} does not parse: {error}"
        ))),
    }
}

/// The probe's exit code and stdout.
async fn report(docker: &impl DockerApi, probe: &str) -> Result<(i64, Vec<u8>), LaunchError> {
    let mut output = docker
        .attach(probe)
        .await
        .map_err(|source| docker_error("attach to the probe", source))?;
    docker
        .start_container(probe)
        .await
        .map_err(|source| docker_error("start the probe", source))?;
    let mut stdout = Vec::new();
    while let Some(piece) = output.recv().await {
        match piece.map_err(|source| docker_error("read the probe's output", source))? {
            Output::Stdout(bytes) => {
                if stdout.len() + bytes.len() > MAX_OUTPUT {
                    return Err(LaunchError::Probe(format!(
                        "more than {MAX_OUTPUT} bytes of output"
                    )));
                }
                stdout.extend_from_slice(&bytes);
            }
            Output::Stderr(bytes) => tracing::warn!(
                probe = short(probe),
                stderr = %String::from_utf8_lossy(&bytes).trim_end(),
                "the probe wrote to stderr"
            ),
        }
    }
    // After the start, as for the daemon: `not-running` answers at once for a created container.
    let code = docker
        .wait(probe)
        .await
        .map_err(|source| docker_error("wait for the probe", source))?
        .await
        .map_err(|_closed| LaunchError::Probe("the wait ended without a status".to_owned()))?
        .map_err(|source| docker_error("wait for the probe", source))?;
    Ok((code, stdout))
}
