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
use std::process::ExitCode;

use swarm_rootless_docker::cli::{Cli, Command};
use swarm_rootless_docker::config::LaunchConfig;
use swarm_rootless_docker::probe::{self, ProbeReport};

/// Exit code of a launcher error.
const EXIT_FAILURE: u8 = 1;
/// Exit code of a usage or configuration error (clap uses the same for its own).
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse_args();
    match cli.command {
        Command::Launch(args) => match LaunchConfig::new(args.into_values()) {
            Ok(_config) => {
                let _ = writeln!(
                    io::stderr(),
                    "swarm-rootless-docker: launch: the launcher is not implemented yet"
                );
                ExitCode::from(EXIT_FAILURE)
            }
            Err(config_error) => {
                let _ = writeln!(
                    io::stderr(),
                    "swarm-rootless-docker: invalid configuration: {config_error}"
                );
                ExitCode::from(EXIT_USAGE)
            }
        },
        Command::Probe { request } => probe(&request),
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
