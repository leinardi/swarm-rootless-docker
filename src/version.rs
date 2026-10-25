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

//! The build's version strings, set at compile time through `SWARM_ROOTLESS_DOCKER_VERSION`,
//! `SWARM_ROOTLESS_DOCKER_COMMIT` and `SWARM_ROOTLESS_DOCKER_DATE` (`make rust-run` and the image
//! build pass them), and logged once at startup.

/// The release version, `dev` for a local build.
pub const VERSION: &str = match option_env!("SWARM_ROOTLESS_DOCKER_VERSION") {
    Some(version) => version,
    None => "dev",
};

/// The commit built, `none` when unknown.
pub const COMMIT: &str = match option_env!("SWARM_ROOTLESS_DOCKER_COMMIT") {
    Some(commit) => commit,
    None => "none",
};

/// The build date, `unknown` when unknown.
pub const DATE: &str = match option_env!("SWARM_ROOTLESS_DOCKER_DATE") {
    Some(date) => date,
    None => "unknown",
};
