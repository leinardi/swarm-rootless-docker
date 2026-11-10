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
//! `SWARM_ROOTLESS_DOCKER_COMMIT` and `SWARM_ROOTLESS_DOCKER_DATE` (`make rust-dist`,
//! `make rust-run` and the image build pass them), and logged once at startup. service-kit's
//! `buildinfo!` reads them; the defaults and the empty-stamp rule are this program's.

use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};

/// What an unstamped build reports: the version `dev`, the commit `none` and the build date
/// `unknown`, and a variable set to the empty string stamps the empty string.
const DEFAULTS: Defaults = Defaults {
    version: "dev",
    commit: "none",
    date: "unknown",
    empty: EmptyStamp::Kept,
};

/// The build's version, commit and date, stamped or defaulted.
pub const BUILD: BuildInfo = buildinfo!("SWARM_ROOTLESS_DOCKER", DEFAULTS);

/// The version line, without the program name: `VERSION (COMMIT, DATE)`. `--version` prints it
/// after the name.
#[must_use]
pub fn long() -> String {
    let BuildInfo {
        version,
        commit,
        date,
    } = BUILD;
    format!("{version} ({commit}, {date})")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults of an unstamped build are pinned as literals, whatever the test build was
    /// stamped with: `cli`'s `--version` test reads `BUILD`, so it cannot catch a wrong default.
    #[test]
    fn defaults_are_dev_none_unknown() {
        assert_eq!(
            DEFAULTS,
            Defaults {
                version: "dev",
                commit: "none",
                date: "unknown",
                empty: EmptyStamp::Kept,
            }
        );
        assert_eq!(
            BuildInfo::resolve(None, None, None, DEFAULTS),
            BuildInfo {
                version: "dev",
                commit: "none",
                date: "unknown",
            }
        );
    }

    /// An empty stamp is kept as the empty string, not replaced by the default.
    #[test]
    fn empty_stamp_is_kept() {
        assert_eq!(DEFAULTS.empty, EmptyStamp::Kept);
        assert_eq!(
            BuildInfo::resolve(Some(""), Some(""), Some(""), DEFAULTS),
            BuildInfo {
                version: "",
                commit: "",
                date: "",
            }
        );
    }
}
