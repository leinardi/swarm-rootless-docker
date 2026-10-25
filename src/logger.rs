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
//! its queue is full, events are dropped.

use std::io;

use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
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
    tracing::subscriber::set_global_default(subscriber(level, format, lane.clone()))
        .map_err(|_| InitError::Installed)?;
    Ok(lane)
}

/// The subscriber [`init`] installs, writing to `writer`; tests install it in a scope.
pub fn subscriber<Writer>(
    level: LogLevel,
    format: LogFormat,
    writer: Writer,
) -> Box<dyn Subscriber + Send + Sync>
where
    Writer: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_max_level(LevelFilter::from(level))
        .with_ansi(false)
        .with_writer(writer);
    match format {
        LogFormat::Text => Box::new(builder.finish()),
        LogFormat::Json => Box::new(builder.json().flatten_event(true).finish()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture(level: LogLevel, format: LogFormat, emit: impl FnOnce()) -> String {
        let captured = Captured::default();
        let writer = captured.clone();
        tracing::subscriber::with_default(subscriber(level, format, move || writer.clone()), emit);
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
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
}
