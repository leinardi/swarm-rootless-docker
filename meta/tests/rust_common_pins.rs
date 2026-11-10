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

//! The rust-common crates are a git dependency pinned by tag, one line per crate under the root
//! `[workspace.dependencies]`, and nothing in cargo checks that the lines agree: a tag-only bump
//! fails to resolve against the old `version`, and a missed line mixes two checkouts. The check is
//! `repo_meta::git_pins`'s, a function of the manifest text, so it is also run on a manifest
//! altered to break it: the negative control.

use repo_meta::git_pins::{RUST_COMMON_URL, git_pins, root_git_pins};
use repo_meta::report;

/// Every rust-common dependency in the root `Cargo.toml` has a `tag` of the form `vX.Y.Z` and a
/// `version` equal to its `X.Y`, all the lines share one tag, and there is at least one line
/// (`repo_meta::git_pins::root_git_pins`).
#[test]
fn the_rust_common_pins_agree() {
    let problems = root_git_pins(&meta::repo(), RUST_COMMON_URL);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Negative control: the root manifest with each of three lines altered is refused, each for its
/// own reason: a tag moved alone, a `version` left behind, and a whole pin (tag and version) moved
/// on one line only, so that two tags are in use.
#[test]
fn an_altered_manifest_is_refused() {
    let text = meta::repo()
        .try_read("Cargo.toml")
        .unwrap_or_else(|problem| panic!("read Cargo.toml: {problem:?}"));
    assert!(
        git_pins(&[("Cargo.toml", &text)], RUST_COMMON_URL).is_empty(),
        "the manifest under test must pass before it is altered"
    );
    let needle = format!("git = \"{RUST_COMMON_URL}\"");
    let lines: Vec<&str> = text.lines().filter(|line| line.contains(&needle)).collect();
    assert!(lines.len() >= 3, "need three rust-common lines to alter");

    let refused = |index: usize, from: &str, to: &str, expected: &str| {
        let altered_line = lines[index].replacen(from, to, 1);
        assert_ne!(
            altered_line, lines[index],
            "the line did not change: {from}"
        );
        let altered = text.replacen(lines[index], &altered_line, 1);
        let problems = git_pins(&[("Cargo.toml", &altered)], RUST_COMMON_URL);
        assert!(
            problems
                .iter()
                .any(|problem| problem.to_string().contains(expected)),
            "expected {expected:?} for {altered_line}, got: {}",
            report(&problems)
        );
    };
    refused(0, "tag = \"v", "tag = \"v9", "has version");
    refused(1, "version = \"", "version = \"0.", "has version");
    // Both halves of the pin move to a release line no consumer uses, so only the tags differ.
    let pin = regex::Regex::new(r#"tag = "[^"]*", version = "[^"]*""#)
        .expect("the pin pattern compiles")
        .find(lines[2])
        .expect("a tag and a version on the line")
        .as_str();
    refused(
        2,
        pin,
        r#"tag = "v0.0.1", version = "0.0""#,
        "pin different tags",
    );
}
