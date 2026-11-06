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

//! Drives scripts/check-license-headers.sh: on this repository, and on scratch repositories that
//! hold each kind of file it must reject or let through. The harness is
//! `repo_meta::scratch::HeaderHarness`: its scratch repositories run git and the script with an
//! isolated git environment, so under the commit hook they never touch the index being
//! committed, and it refuses a set of cases that lacks one to accept or one to reject.

use repo_meta::report;
use repo_meta::scratch::{HeaderCase, HeaderHarness};

const SCRIPT_PATH: &str = "scripts/check-license-headers.sh";

/// The MIT notice without its comment prefix, as the repository's files carry it.
const NOTICE: &[&str] = &[
    "MIT License",
    "",
    "Copyright (c) 2026 Roberto Leinardi",
    "",
    "Permission is hereby granted, free of charge, to any person obtaining a copy",
    "of this software and associated documentation files (the \"Software\"), to deal",
    "in the Software without restriction, including without limitation the rights",
    "to use, copy, modify, merge, publish, distribute, sublicense, and/or sell",
    "copies of the Software, and to permit persons to whom the Software is",
    "furnished to do so, subject to the following conditions:",
    "",
    "The above copyright notice and this permission notice shall be included in all",
    "copies or substantial portions of the Software.",
    "",
    "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR",
    "IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,",
    "FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE",
    "AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER",
    "LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,",
    "OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE",
    "SOFTWARE.",
];

/// The line of a Rust header that holds the last notice line the checker requires (the warranty
/// disclaimer's first line), counted from 1.
const LAST_REQUIRED_LINE: usize = 16;

/// The number of leading lines the checker reads.
const HEADER_WINDOW: usize = 30;

/// Scratch files under this prefix are written but never added: the check reads tracked files only.
const UNTRACKED_PREFIX: &str = "untracked/";

/// The notice as a comment block: an opening line, each notice line with `prefix` (or `blank` for
/// an empty one), and a closing line.
fn comment(opening: &str, prefix: &str, blank: &str, closing: &str) -> String {
    let mut lines = vec![opening.to_owned()];
    for line in NOTICE {
        if line.is_empty() {
            lines.push(blank.to_owned());
        } else {
            lines.push(format!("{prefix}{line}"));
        }
    }
    lines.push(closing.to_owned());
    lines.join("\n") + "\n"
}

fn rust_header() -> String {
    comment("/*", " * ", " *", " */") + "\npub fn run() {}\n"
}

fn hash_header() -> String {
    comment("#", "# ", "#", "#")
}

/// The scratch repositories' Makefile, which the check governs too and reads `MK_COMMON_FILES`
/// from, unless a case brings its own.
fn base_makefile() -> String {
    hash_header() + "\nMK_COMMON_FILES := docker.mk help.mk rust.mk\n"
}

/// Go: `TestRepositoryHeaders`. The repository passes its own check. Run with the repository's
/// own git environment: under the commit hook it names the index being committed.
#[test]
fn repository_headers() {
    let harness = HeaderHarness {
        script: SCRIPT_PATH,
        base_files: &[],
        untracked_prefix: UNTRACKED_PREFIX,
    };
    let problems = harness.check_repository(&meta::repo());
    assert!(problems.is_empty(), "{}", report(&problems));
}

fn case(name: &str, files: &[(&str, &str)], pass: bool) -> HeaderCase {
    HeaderCase::new(name, files, pass)
}

/// Runs the cases in scratch repositories, each with the checker copied from this repository and
/// the base Makefile unless the case brings its own.
fn run_cases(cases: &[HeaderCase]) {
    let makefile = base_makefile();
    let harness = HeaderHarness {
        script: SCRIPT_PATH,
        base_files: &[("Makefile", &makefile)],
        untracked_prefix: UNTRACKED_PREFIX,
    };
    let problems = harness.run_cases(&meta::repo(), cases);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestHeaderCases`, the notice itself on a Rust file: present, missing, a year range,
/// another holder, cut short, and the edges of the window it must sit in.
#[test]
fn header_cases_for_the_notice() {
    let rust = rust_header();
    let filler = "// filler\n".repeat(HEADER_WINDOW - LAST_REQUIRED_LINE);
    run_cases(&[
        case("rust file with the header", &[("src/x.rs", &rust)], true),
        case(
            "rust file without a header",
            &[("src/x.rs", "pub fn run() {}\n")],
            false,
        ),
        case(
            "a year range",
            &[("src/x.rs", &rust.replacen("2026", "2025-2026", 1))],
            true,
        ),
        case(
            "another copyright holder",
            &[(
                "src/x.rs",
                &rust.replacen("Roberto Leinardi", "Someone Else", 1),
            )],
            false,
        ),
        case(
            "the notice cut short",
            &[("src/x.rs", &rust.replacen(" * THE SOFTWARE IS", " *", 1))],
            false,
        ),
        case(
            "header ending on the last line of the window",
            &[("src/x.rs", &format!("{filler}{rust}"))],
            true,
        ),
        case(
            "header ending one line past the window",
            &[("src/x.rs", &format!("// filler\n{filler}{rust}"))],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, every governed file kind with and without the header.
#[test]
fn header_cases_for_each_file_kind() {
    let shell = format!("#!/usr/bin/env bash\n{}\necho ok\n", hash_header());
    let dockerfile = format!(
        "# syntax=docker/dockerfile:1.7\n\n{}FROM scratch\n",
        hash_header()
    );
    let unlisted_rust = hash_header() + "\nMK_COMMON_FILES := docker.mk help.mk\n";
    run_cases(&[
        case(
            "shell script with the header",
            &[("scripts/a.sh", &shell)],
            true,
        ),
        case(
            "shell script without a header",
            &[("scripts/a.sh", "#!/usr/bin/env bash\necho\n")],
            false,
        ),
        case(
            "dockerfile after its directive",
            &[("deployments/docker/Dockerfile", &dockerfile)],
            true,
        ),
        case(
            "dockerfile without a header",
            &[("deployments/docker/Dockerfile", "FROM scratch\n")],
            false,
        ),
        case(
            "local make fragment, no header",
            &[(".mk/verify.mk", "x:\n")],
            false,
        ),
        case(
            "make-common name the Makefile does not list, no header",
            &[(".mk/rust.mk", "x:\n"), ("Makefile", &unlisted_rust)],
            false,
        ),
        case(
            "makefile without a header",
            &[("Makefile", "MK_COMMON_FILES := docker.mk\n")],
            false,
        ),
    ]);
}

/// Go: `TestHeaderCases`, what is never governed: untracked files, make-common's files, and
/// Markdown, YAML and JSON.
#[test]
fn header_cases_for_the_exemptions() {
    run_cases(&[
        case(
            "untracked file without a header",
            &[("untracked/scratch.rs", "pub fn run() {}\n")],
            true,
        ),
        case(
            "upstream make fragment",
            &[
                (".mk/docker.mk", "x:\n"),
                (".mk/help.mk", "y:\n"),
                (".mk/rust.mk", "z:\n"),
            ],
            true,
        ),
        case(
            "bootstrap script",
            &[("scripts/bootstrap-mk-common.sh", "#!/bin/sh\n")],
            true,
        ),
        case(
            "markdown, yaml and json",
            &[
                ("README.md", "# x\n"),
                ("a.yaml", "a: 1\n"),
                ("b.json", "{}\n"),
            ],
            true,
        ),
        // The negative control of the exemptions: the bootstrap script is exempt by its exact
        // path, so the same name anywhere else is governed.
        case(
            "the bootstrap script's name in another directory, no header",
            &[("src/bootstrap-mk-common.sh", "#!/bin/sh\n")],
            false,
        ),
    ]);
}
