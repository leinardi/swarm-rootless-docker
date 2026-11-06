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

//! The shared lint set is declared once, in the root `[workspace.lints]`, and a package only obeys
//! it when its own manifest opts in: `[lints]` is not inherited by workspace members. A member that
//! forgets `[lints] workspace = true` builds and passes clippy with the default lints, silently.
//!
//! The checks are `repo_meta::workspace`'s, functions of the manifests, so each one is also run on
//! manifests built to break it: the negative controls.

use repo_meta::workspace::{lint_opt_in, member_opt_in_problems, root_lint_problems};
use repo_meta::{Problem, report, strings};
use toml::Table;

/// The workspace's members, exactly: the launcher and the meta crate.
const MEMBERS: &[&str] = &[".", "meta"];

fn parse(text: &str, origin: &str) -> Table {
    text.parse::<Table>()
        .unwrap_or_else(|error| panic!("parse {origin}: {error}"))
}

/// The root manifest declares `[workspace.lints.rust]` and `[workspace.lints.clippy]`, lists its
/// members explicitly (no glob, exactly [`MEMBERS`]), and every member declares `[lints] workspace
/// = true` (`repo_meta::workspace::lint_opt_in`).
#[test]
fn every_workspace_member_opts_into_the_workspace_lints() {
    let problems = lint_opt_in(&meta::repo(), Some(MEMBERS));
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: a root without the lint tables, or with only one of them, is refused.
#[test]
fn a_root_without_workspace_lints_is_refused() {
    let missing = parse("[workspace]\nmembers = [\".\"]\n", "fixture");
    assert_eq!(
        strings(&root_lint_problems(&missing)),
        ["the root Cargo.toml has no [workspace.lints] table"]
    );
    let clippy_only = parse("[workspace.lints.clippy]\npedantic = \"warn\"\n", "fixture");
    assert_eq!(
        strings(&root_lint_problems(&clippy_only)),
        ["[workspace.lints.rust] is missing"]
    );
    let rust_only = parse(
        "[workspace.lints.rust]\nunsafe_code = \"forbid\"\n",
        "fixture",
    );
    assert_eq!(
        strings(&root_lint_problems(&rust_only)),
        ["[workspace.lints.clippy] is missing"]
    );
}

/// Negative control: a member that does not opt in, one that opts out, and a glob are each
/// refused, while the member that opts in passes; a member list other than the expected one is
/// refused too.
#[test]
fn a_member_without_the_opt_in_is_refused() {
    let root = parse(
        "[workspace]\nmembers = [\".\", \"silent\", \"opted-out\", \"crates/*\"]\n",
        "fixture",
    );
    let problems = member_opt_in_problems(
        &root,
        |member| -> Result<Table, Problem> {
            Ok(match member {
                "." => parse("[lints]\nworkspace = true\n", member),
                "opted-out" => parse("[lints]\nworkspace = false\n", member),
                _ => parse("[package]\nname = \"silent\"\n", member),
            })
        },
        Some(MEMBERS),
    );
    assert_eq!(
        strings(&problems),
        [
            "silent/Cargo.toml does not declare [lints] workspace = true",
            "opted-out/Cargo.toml does not declare [lints] workspace = true",
            "member \"crates/*\" is a glob; list members explicitly so this check sees each one",
            "the workspace members are [\".\", \"silent\", \"opted-out\"]; want exactly [\".\", \"meta\"]",
        ]
    );
}

/// Negative control: a root with no members, and a member whose manifest cannot be read, are
/// problems, not passes.
#[test]
fn an_unreadable_or_missing_member_is_refused() {
    let empty = parse("[workspace]\nmembers = []\n", "fixture");
    assert_eq!(
        strings(&member_opt_in_problems(&empty, |_| Ok(Table::new()), None)),
        ["the workspace lists no members"]
    );
    let root = parse("[workspace]\nmembers = [\"gone\"]\n", "fixture");
    let problems = member_opt_in_problems(
        &root,
        |member| {
            Err(Problem::at(
                format!("{member}/Cargo.toml"),
                "cannot be read",
            ))
        },
        None,
    );
    assert_eq!(strings(&problems), ["gone/Cargo.toml: cannot be read"]);
}
