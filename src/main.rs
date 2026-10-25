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

//! The swarm-rootless-docker binary: parses the command line and runs the chosen mode.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use swarm_rootless_docker::cli::{Cli, Command};
use swarm_rootless_docker::config::LaunchConfig;
use swarm_rootless_docker::launcher::{self, Console};
use swarm_rootless_docker::logger::{self, LogFormat, LogLevel};
use swarm_rootless_docker::probe::{self, ProbeReport};
use swarm_rootless_docker::{daemon, status};

/// Exit code of a launcher error.
const EXIT_FAILURE: u8 = 1;
/// Exit code of a usage or configuration error (clap uses the same for its own).
const EXIT_USAGE: u8 = 2;
/// How long the tasks still running when the launcher ends get.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);
/// How long the output still queued at the end gets, per stream.
const OUTPUT_FLUSH: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let cli = Cli::parse_args();
    match cli.command {
        Command::Launch(args) => {
            let (level, format) = (args.log_level, args.log_format);
            match LaunchConfig::new(args.into_values()) {
                Ok(config) => launch(&config, level, format),
                Err(config_error) => {
                    let _ = writeln!(
                        io::stderr(),
                        "swarm-rootless-docker: invalid configuration: {config_error}"
                    );
                    ExitCode::from(EXIT_USAGE)
                }
            }
        }
        Command::Probe { request } => probe(&request),
        Command::Daemon => {
            let error = daemon::run();
            let _ = writeln!(io::stderr(), "swarm-rootless-docker: daemon: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
        Command::DaemonHealth => report_health(daemon::health().map_err(|error| error.to_string())),
        Command::Healthcheck => report_health(status::query(
            Path::new(status::STATUS_SOCKET),
            status::QUERY_TIMEOUT,
        )),
    }
}

/// Runs the launcher on a single-threaded runtime, ended with a timeout so a task that is still
/// blocked cannot hold the process open.
fn launch(config: &LaunchConfig, level: LogLevel, format: LogFormat) -> ExitCode {
    let log = match logger::init(level, format) {
        Ok(log) => log,
        Err(error) => {
            let _ = writeln!(io::stderr(), "swarm-rootless-docker: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let console = match Console::process() {
        Ok(console) => console,
        Err(error) => {
            let _ = writeln!(
                io::stderr(),
                "swarm-rootless-docker: start the output writers: {error}"
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "build the runtime");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let code = runtime.block_on(launcher::launch(config, console.clone()));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    // The last of the daemon's output and the launcher's own last lines, unless a stream is
    // stuck: then they are lost, and the process still ends.
    let _ = console.flush(OUTPUT_FLUSH);
    let _ = log.flush(OUTPUT_FLUSH);
    ExitCode::from(code)
}

/// A healthcheck's result: exit 0, or the reason on stderr and exit 1.
fn report_health(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            let _ = writeln!(io::stderr(), "swarm-rootless-docker: unhealthy: {reason}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Prints the probe's report as one JSON line on stdout: exit 0 when accepted, 1 when refused
/// (or when the report cannot be written, which the launcher reads as a refusal too).
fn probe(request: &str) -> ExitCode {
    let report = probe::run_on_host(request);
    let accepted = matches!(report, ProbeReport::Accepted { .. });
    let written = serde_json::to_string(&report)
        .map_err(io::Error::other)
        .and_then(|line| writeln!(io::stdout(), "{line}"));
    if accepted && written.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_FAILURE)
    }
}
