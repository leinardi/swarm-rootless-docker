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

//! One request and its whole answer over a Unix socket, under one deadline: what the two
//! healthchecks do.
//!
//! Per-call timeouts on a blocking socket do not bound the whole: a connect to a listener whose
//! backlog is full blocks before any timeout applies, and a read timeout starts again with every
//! byte, so a trickled answer never trips it. So the exchange runs on a small runtime, its
//! connect is non-blocking (a full backlog fails at once), and one timeout covers the connect, the
//! write and the read to the end.

#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// Connects to `path`, writes `request`, and reads the answer to its end (at most `max_answer`
/// bytes), all within `deadline`.
///
/// # Errors
///
/// [`io::ErrorKind::TimedOut`] when the deadline passes first, or the I/O error of any step.
pub fn exchange(
    path: &Path,
    request: &[u8],
    max_answer: u64,
    deadline: Duration,
) -> io::Result<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let result = runtime.block_on(async {
        let exchange = async {
            let mut stream = UnixStream::connect(path).await?;
            stream.write_all(request).await?;
            let mut answer = Vec::new();
            stream.take(max_answer).read_to_end(&mut answer).await?;
            Ok::<_, io::Error>(answer)
        };
        tokio::time::timeout(deadline, exchange)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no complete answer within {deadline:?}"),
                ))
            })
    });
    runtime.shutdown_background();
    result
}
