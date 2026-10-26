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

//! The command line: one subcommand per mode of the one binary. Every per-instance flag of
//! `launch` is parsed straight into its validated type (`config`), so clap reports a bad value
//! with the flag's name, and a flag given twice is an error rather than "last one wins".

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::config::{
    ExtraMount, HostPath, IdRange, InstanceName, LaunchValues, MemoryBytes, NanoCpus, NetworkName,
    PidsLimit,
};
use crate::logger::{LogFormat, LogLevel};
use crate::version;

/// The binary's command line.
#[derive(Debug, Parser)]
#[command(
    name = "swarm-rootless-docker",
    about = "Launcher and image for a dedicated rootless Docker daemon per Docker Swarm service",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parses the process's arguments, exiting with clap's usage error (code 2), or after help
    /// or the version, as clap does.
    #[must_use]
    pub fn parse_args() -> Self {
        Self::from_arg_matches(&command().get_matches()).unwrap_or_else(|error| error.exit())
    }
}

/// The command definition, with `--version` reporting the version, commit and build date
/// (`version::long`). Set at run time because clap's derive takes only a constant string.
#[must_use]
pub fn command() -> clap::Command {
    Cli::command().version(version::long())
}

/// The binary's modes.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create, supervise and stop this instance's daemon container (the Swarm service mode).
    Launch(LaunchArgs),
    /// Check the host paths and ranges of a request, in the probe container the launcher creates
    /// (internal: the request is the launcher's JSON).
    #[command(hide = true)]
    Probe {
        /// The request, as JSON.
        request: String,
    },
    /// Start the rootless dockerd, in the daemon container (internal: takes no argument).
    #[command(hide = true)]
    Daemon,
    /// The daemon container's healthcheck: ping dockerd (internal).
    #[command(hide = true)]
    DaemonHealth,
    /// The launcher container's healthcheck: ask the launcher (the image's `HEALTHCHECK`).
    #[command(hide = true)]
    Healthcheck,
}

/// The flags of `launch`: what differs between consumers, and nothing about the daemon's
/// security profile, which is fixed.
#[derive(Debug, Args)]
pub struct LaunchArgs {
    /// Instance name; the daemon container is swarm-rootless-docker-daemon-NAME.
    #[arg(long, value_name = "NAME", value_parser = InstanceName::parse)]
    pub instance: InstanceName,
    /// Subordinate UID range of the daemon, START:COUNT.
    #[arg(long, value_name = "START:COUNT", value_parser = IdRange::parse)]
    pub subuid: IdRange,
    /// Subordinate GID range of the daemon, START:COUNT (default: the --subuid range).
    #[arg(long, value_name = "START:COUNT", value_parser = IdRange::parse)]
    pub subgid: Option<IdRange>,
    /// Host directory holding the daemon's data root.
    #[arg(long, value_name = "PATH", value_parser = HostPath::parse)]
    pub data_root: HostPath,
    /// Host directory the daemon's socket is created in; consumers mount it read-only.
    #[arg(long, value_name = "PATH", value_parser = HostPath::parse)]
    pub socket_dir: HostPath,
    /// Extra bind mount SOURCE:TARGET[:ro|:rw], e.g. the consumer's workspace at the same path.
    #[arg(long = "mount", value_name = "SOURCE:TARGET[:ro]", value_parser = ExtraMount::parse)]
    pub mounts: Vec<ExtraMount>,
    /// Docker network the daemon container joins.
    #[arg(long, value_name = "NAME", value_parser = NetworkName::parse)]
    pub network: NetworkName,
    /// Memory limit of the daemon container (bytes, or with a k, m or g suffix); also its swap.
    #[arg(long, value_name = "SIZE", value_parser = MemoryBytes::parse)]
    pub memory: MemoryBytes,
    /// Pids limit of the daemon container.
    #[arg(long, value_name = "N", value_parser = PidsLimit::parse)]
    pub pids_limit: PidsLimit,
    /// CPU limit of the daemon container, in CPUs (e.g. 2 or 1.5).
    #[arg(long, value_name = "CPUS", value_parser = NanoCpus::parse)]
    pub cpus: Option<NanoCpus>,
    /// The launcher's own log level.
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,
    /// The launcher's own log format, on stderr.
    #[arg(long, value_enum, default_value_t = LogFormat::Text)]
    pub log_format: LogFormat,
}

impl LaunchArgs {
    /// The parsed values, for [`crate::config::LaunchConfig::new`] to check as a whole.
    #[must_use]
    pub fn into_values(self) -> LaunchValues {
        LaunchValues {
            instance: self.instance,
            subuid: self.subuid,
            subgid: self.subgid,
            data_root: self.data_root,
            socket_dir: self.socket_dir,
            mounts: self.mounts,
            network: self.network,
            memory: self.memory,
            pids_limit: self.pids_limit,
            cpus: self.cpus,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    const REQUIRED: &[&str] = &[
        "swarm-rootless-docker",
        "launch",
        "--instance",
        "herdquay",
        "--subuid",
        "100000:65536",
        "--data-root",
        "/mnt/pool/data",
        "--socket-dir",
        "/mnt/pool/socket",
        "--network",
        "herdquay_docker",
        "--memory",
        "8g",
        "--pids-limit",
        "8192",
    ];

    fn parse(extra: &[&str]) -> Result<Cli, clap::Error> {
        let matches = command().try_get_matches_from(REQUIRED.iter().chain(extra))?;
        Cli::from_arg_matches(&matches)
    }

    #[test]
    fn command_definition_is_consistent() {
        command().debug_assert();
    }

    /// `--version` (and `-V`) prints the name, the version, the commit and the build date.
    #[test]
    fn version_reports_commit_and_date() {
        for flag in ["--version", "-V"] {
            let error = command()
                .try_get_matches_from(["swarm-rootless-docker", flag])
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::DisplayVersion, "{flag}");
            assert_eq!(
                error.to_string(),
                format!(
                    "swarm-rootless-docker {} ({}, {})\n",
                    version::VERSION,
                    version::COMMIT,
                    version::DATE
                ),
                "{flag}"
            );
        }
    }

    #[test]
    fn required_flags_are_enough() {
        let Command::Launch(args) = parse(&[]).unwrap().command else {
            panic!("not launch");
        };
        assert_eq!(args.instance.as_str(), "herdquay");
        assert!(args.subgid.is_none() && args.cpus.is_none() && args.mounts.is_empty());
    }

    /// The subcommands the container specs run are the ones this binary has, and take no argument.
    #[test]
    fn spec_commands_exist() {
        use crate::spec::{DAEMON_COMMAND, DAEMON_HEALTH_COMMAND};
        let parse = |argv: &[&str]| {
            let matches = command().try_get_matches_from(argv)?;
            Cli::from_arg_matches(&matches)
        };
        let cli = parse(&["swarm-rootless-docker", DAEMON_COMMAND]).unwrap();
        assert!(matches!(cli.command, Command::Daemon));
        let cli = parse(&["swarm-rootless-docker", DAEMON_HEALTH_COMMAND]).unwrap();
        assert!(matches!(cli.command, Command::DaemonHealth));
        let cli = parse(&["swarm-rootless-docker", "healthcheck"]).unwrap();
        assert!(matches!(cli.command, Command::Healthcheck));
        for argv in [
            &["swarm-rootless-docker", DAEMON_COMMAND, "--debug"][..],
            &[
                "swarm-rootless-docker",
                DAEMON_COMMAND,
                "-H",
                "tcp://0.0.0.0:2375",
            ],
            &["swarm-rootless-docker", DAEMON_HEALTH_COMMAND, "x"],
        ] {
            assert!(parse(argv).is_err(), "{argv:?}");
        }
    }

    /// `probe` takes exactly one argument and stays out of the help: it is the launcher's.
    #[test]
    fn probe_is_internal() {
        let matches = command()
            .try_get_matches_from(["swarm-rootless-docker", "probe", "{}"])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        assert!(matches!(cli.command, Command::Probe { request } if request == "{}"));
        for extra in [&["probe"][..], &["probe", "{}", "{}"]] {
            let argv = ["swarm-rootless-docker"].iter().chain(extra);
            assert!(command().try_get_matches_from(argv).is_err(), "{extra:?}");
        }
        let help = command().render_help().to_string();
        for internal in ["probe", "daemon", "daemon-health", "healthcheck"] {
            assert!(!help.contains(&format!("\n  {internal} ")), "{help}");
        }
    }

    #[test]
    fn log_flags_default_to_info_text() {
        let Command::Launch(args) = parse(&[]).unwrap().command else {
            panic!("not launch");
        };
        assert_eq!(
            (args.log_level, args.log_format),
            (LogLevel::Info, LogFormat::Text)
        );
        let Command::Launch(args) = parse(&["--log-level", "debug", "--log-format", "json"])
            .unwrap()
            .command
        else {
            panic!("not launch");
        };
        assert_eq!(
            (args.log_level, args.log_format),
            (LogLevel::Debug, LogFormat::Json)
        );
        let error = parse(&["--log-level", "verbose"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn mounts_repeat() {
        let Command::Launch(args) =
            parse(&["--mount", "/mnt/a:/srv/a", "--mount", "/mnt/b:/srv/b:ro"])
                .unwrap()
                .command
        else {
            panic!("not launch");
        };
        assert_eq!(args.mounts.len(), 2);
    }

    #[test]
    fn every_required_flag_is_required() {
        for (index, flag) in REQUIRED.iter().enumerate().skip(2).step_by(2) {
            let without: Vec<&str> = REQUIRED
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index && *position != index + 1)
                .map(|(_, value)| *value)
                .collect();
            let error = command().try_get_matches_from(without).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument, "{flag}");
        }
    }

    /// A flag given twice is an error, never "last one wins": a compose file that appends a second
    /// --data-root must not silently replace the first.
    #[test]
    fn single_flags_refuse_repetition() {
        for (flag, value) in [
            ("--instance", "other"),
            ("--subuid", "200000:65536"),
            ("--data-root", "/mnt/other"),
            ("--network", "other"),
            ("--memory", "1g"),
        ] {
            let error = parse(&[flag, value]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{flag}");
        }
    }

    /// Nothing about the daemon's profile is a flag.
    #[test]
    fn profile_flags_do_not_exist() {
        for flag in [
            "--privileged",
            "--cap-add",
            "--device",
            "--security-opt",
            "--user",
            "--pid",
            "--userns",
            "--image",
            "--host-network",
        ] {
            let error = parse(&[flag, "x"]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{flag}");
        }
    }

    #[test]
    fn invalid_values_name_the_flag() {
        let error = parse(&["--mount", "/etc:/srv/etc"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
        assert!(error.to_string().contains("--mount"), "{error}");
    }

    #[test]
    fn no_subcommand_is_an_error() {
        let error = command()
            .try_get_matches_from(["swarm-rootless-docker"])
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }
}
