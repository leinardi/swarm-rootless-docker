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

//! The launcher's own logging: one global `tracing` subscriber writing to stderr, as text or
//! JSON. Stdout carries the daemon's stdout, unmixed.
//!
//! The writes go through a [`Lane`], so a stderr nobody reads never blocks the runtime; while
//! its queue is full, events are dropped. The subscriber is rust-common's `tracing-setup`, with
//! the target and the JSON span fields shown; the flags, the errors and the writer are here.

use std::io;

use tracing::Dispatch;
use tracing::level_filters::LevelFilter;
use tracing_setup::Options;
use tracing_subscriber::fmt::MakeWriter;

use crate::output::Lane;

/// The `--log-level` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

/// The `--log-format` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    /// tracing-subscriber's text, the fields as `key=value`.
    Text,
    /// One JSON object per line, the event's fields at the top level.
    Json,
}

/// The logger could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("a global logger is already installed")]
    Installed,
    #[error("start the log writer: {0}")]
    Writer(io::Error),
}

/// Installs the global subscriber, writing to stderr through a [`Lane`], which it returns for a
/// last flush at exit.
///
/// # Errors
///
/// An [`InitError`] when one is already installed or the writer cannot start.
pub fn init(level: LogLevel, format: LogFormat) -> Result<Lane, InitError> {
    let lane = Lane::spawn("log", io::stderr()).map_err(InitError::Writer)?;
    tracing_setup::install_global(subscriber(level, format, lane.clone()))
        .map_err(|_| InitError::Installed)?;
    Ok(lane)
}

/// The subscriber [`init`] installs, writing to `writer`; tests install it in a scope.
pub fn subscriber<Writer>(level: LogLevel, format: LogFormat, writer: Writer) -> Dispatch
where
    Writer: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let format = match format {
        LogFormat::Text => tracing_setup::Format::Text,
        LogFormat::Json => tracing_setup::Format::Json,
    };
    tracing_setup::build(writer, format, LevelFilter::from(level), Options::new())
}

#[cfg(test)]
mod tests {
    use tracing_setup::testing::Capture;

    use super::*;

    fn capture(level: LogLevel, format: LogFormat, emit: impl FnOnce()) -> String {
        let captured = Capture::default();
        tracing::dispatcher::with_default(&subscriber(level, format, captured.clone()), emit);
        captured.text()
    }

    #[test]
    fn the_level_filters() {
        let output = capture(LogLevel::Warn, LogFormat::Text, || {
            tracing::info!("hidden");
            tracing::warn!("shown");
        });
        assert!(output.contains("shown"), "{output}");
        assert!(!output.contains("hidden"), "{output}");
    }

    #[test]
    fn text_has_key_value_fields_and_no_ansi() {
        let output = capture(LogLevel::Info, LogFormat::Text, || {
            tracing::info!(daemon = "abc", "created the daemon");
        });
        assert!(output.contains(" INFO "), "{output}");
        assert!(output.contains("daemon=\"abc\""), "{output}");
        assert!(!output.contains('\u{1b}'), "ANSI escapes in {output}");
    }

    #[test]
    fn json_is_one_flat_object_per_line() {
        let output = capture(LogLevel::Info, LogFormat::Json, || {
            tracing::info!(daemon = "abc", "created the daemon");
        });
        let line: serde_json::Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(line["message"], "created the daemon");
        assert_eq!(line["daemon"], "abc");
        assert_eq!(line["level"], "INFO");
    }

    /// The JSON line names its target and, under a span, the current span and the span list, so
    /// a change in tracing-setup's defaults that hid either is caught here.
    #[test]
    fn json_names_the_target_and_the_spans() {
        let output = capture(LogLevel::Info, LogFormat::Json, || {
            let span = tracing::info_span!("launch", daemon = "abc");
            let _entered = span.enter();
            tracing::info!("created the daemon");
        });
        let line: serde_json::Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(line["target"], module_path!());
        assert_eq!(line["span"]["name"], "launch");
        assert_eq!(line["spans"][0]["daemon"], "abc");
    }
}
