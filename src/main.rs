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

//! The swarm-rootless-docker binary. A placeholder until the launcher lands: it reports its
//! version and refuses everything else.

mod version;

use std::io::{self, Write};
use std::process::ExitCode;

/// Usage error: no or an unknown subcommand.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let first = std::env::args_os().nth(1);
    if first.as_deref() == Some("--version".as_ref()) {
        let _ = writeln!(
            io::stdout(),
            "swarm-rootless-docker {} ({}, {})",
            version::VERSION,
            version::COMMIT,
            version::DATE
        );
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(io::stderr(), "usage: swarm-rootless-docker --version");
    ExitCode::from(EXIT_USAGE)
}
