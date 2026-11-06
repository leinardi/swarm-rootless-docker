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

//! Repository meta-tests: checks on the repository itself rather than on the launcher. The tests
//! live under `tests/` and run the checks of rust-common's `repo-meta` crate on this repository,
//! each with this repository's parameters, beside the rules that are this repository's own. This
//! library holds what they share: the repository under test, and the gate of the external tools
//! the behavioural tests drive (testenv-core's `toolgate`).

use repo_meta::Repo;
use testenv_core::toolgate::{Gate, Locate, Note};

/// Dockerfile Roast, the Dockerfile linter: the pre-commit hook builds its own, the meta-tests run
/// the one on `PATH`.
pub const DROAST: &str = "droast";

/// The shell linter droast's bridge runs from `PATH`; `droast.toml` makes the bridge required, so
/// droast fails without it.
pub const SHELLCHECK: &str = "shellcheck";

/// The external tools the behavioural meta-tests drive. A behavioural test that quietly passes
/// when its tool is missing passes in exactly the place it is meant to guard: CI. So a missing
/// tool ends the test early, with a note on standard error, on a developer machine, and fails it
/// wherever `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1` is set, which the CI test jobs do. The tools
/// are a closed list: widening it is a decision, not a side effect of a new test.
pub const TOOLS: Gate = Gate {
    require_env: "SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS",
    tools: &[DROAST, SHELLCHECK],
    locate: Locate::Path,
    note: Note::Stderr,
};

/// This repository: the meta crate sits one level below its root. Its git commands keep the
/// process's environment, so under the commit hook its listings name the index being committed.
#[must_use]
pub fn repo() -> Repo {
    Repo::new(testenv_core::repo_root!())
}
