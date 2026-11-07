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

//! The repository's tooling pins, each one value across the files that repeat it: the Rust line
//! (rust-toolchain.toml, Cargo.toml and the image's base tag), the image's Rust release (the base
//! tag and the `RUST_VERSION` argument the musl standard library is fetched for), and
//! `ShellCheck` (the hook and CI); and no flag that makes a gate pass on nothing. The pins are
//! `repo_meta::toolchain::one_value`'s, with this repository's files; the negative controls run
//! them on altered copies.

use repo_meta::report;
use repo_meta::toolchain::{PinSource, one_value};
use repo_meta::vacuous::{AllowedFragment, VacuousFlags};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const CARGO: &str = "Cargo.toml";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const PRE_COMMIT: &str = ".pre-commit-config.yaml";
const SETUP_SHELLCHECK: &str = ".github/actions/setup-shellcheck/action.yaml";
const CI: &str = ".github/workflows/ci.yaml";

/// The `X.Y` line of the build stage's `dhi.io/rust` tag.
const IMAGE_RUST_LINE: &str =
    r"(?mi)^[ \t]*FROM[ \t]+(?:--\S+[ \t]+)*dhi\.io/rust:(\d+\.\d+)\.\d+-";

/// Every YAML file under .github, the workflows and the composite actions; none is a failure, so
/// a scan cannot check nothing.
fn github_yaml() -> Vec<String> {
    let files = meta::repo().files_under(".github", &[".yaml", ".yml"]);
    assert!(
        files.iter().any(|file| file == CI),
        "the scan saw no CI workflow under .github: {files:?}"
    );
    files
}

/// The sources of the Rust line, from the given file contents.
fn rust_line_sources(toolchain: &str, cargo: &str, dockerfile: &str) -> [PinSource; 3] {
    [
        PinSource::rust_channel_line(TOOLCHAIN, toolchain),
        PinSource::rust_version_line(CARGO, cargo),
        PinSource::new(DOCKERFILE, dockerfile, IMAGE_RUST_LINE),
    ]
}

/// The sources of the image's Rust release, from the Dockerfile.
fn image_release_sources(dockerfile: &str) -> [PinSource; 2] {
    [
        PinSource::rust_image_tag(DOCKERFILE, dockerfile),
        PinSource::rust_version_arg(DOCKERFILE, dockerfile),
    ]
}

/// The sources of the `ShellCheck` release, as `X.Y.Z`: shellcheck-py adds a fourth component of
/// its own (v0.11.0.1), so only the first three are compared.
fn shellcheck_sources(hooks: &str, action: &str) -> [PinSource; 2] {
    [
        PinSource::new(
            PRE_COMMIT,
            hooks,
            r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*v(\d+\.\d+\.\d+)",
        ),
        PinSource::new(
            SETUP_SHELLCHECK,
            action,
            r#"SHELLCHECK_VERSION:\s*["']?v(\d+\.\d+\.\d+)"#,
        ),
    ]
}

/// `content` with the first `from` replaced by `to`, which must change it.
fn altered(content: &str, from: &str, to: &str) -> String {
    let changed = content.replacen(from, to, 1);
    assert_ne!(changed, content, "the alteration {from:?} changed nothing");
    changed
}

/// The `X.Y` line is one value in rust-toolchain.toml, the MSRV in Cargo.toml and the image's
/// build stage tag (the image carries its own patch release).
#[test]
fn rust_line_is_one_value() {
    let repo = meta::repo();
    let sources = rust_line_sources(
        &repo.read(TOOLCHAIN),
        &repo.read(CARGO),
        &repo.read(DOCKERFILE),
    );
    let problems = one_value("the Rust line", &sources, &[]);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control of the line: an MSRV, a toolchain and an image tag on another line are
/// each refused, a channel that is not a release leaves a source unwritten, and a later patch of
/// the line is not refused.
#[test]
fn rust_line_control_fails_on_another_line() {
    let repo = meta::repo();
    let (toolchain, cargo, dockerfile) = (
        repo.read(TOOLCHAIN),
        repo.read(CARGO),
        repo.read(DOCKERFILE),
    );
    let check = |toolchain: &str, cargo: &str, dockerfile: &str| {
        one_value(
            "the Rust line",
            &rust_line_sources(toolchain, cargo, dockerfile),
            &[],
        )
        .len()
    };
    assert_eq!(check(&toolchain, &cargo, &dockerfile), 0);
    let line = pin_of(&toolchain, r#"(?m)^channel\s*=\s*"(\d+\.\d+)\.\d+""#);
    assert_eq!(
        check(
            &toolchain,
            &altered(
                &cargo,
                &format!("rust-version = \"{line}\""),
                "rust-version = \"1.0\""
            ),
            &dockerfile
        ),
        1
    );
    assert_eq!(
        check(
            &toolchain,
            &cargo,
            &altered(
                &dockerfile,
                &format!("dhi.io/rust:{line}."),
                "dhi.io/rust:1.0."
            )
        ),
        1
    );
    assert_eq!(
        check(
            &altered(&toolchain, "channel = \"", "channel = \"1.0."),
            &cargo,
            &dockerfile
        ),
        1
    );
    let patched = format!("rust-version = \"{line}.9\"");
    assert_eq!(
        check(
            &toolchain,
            &altered(&cargo, &format!("rust-version = \"{line}\""), &patched),
            &dockerfile
        ),
        0
    );
    assert_eq!(check(&toolchain, "[package]\n", &dockerfile), 1);
}

/// The first group of `pattern` in `content`.
fn pin_of(content: &str, pattern: &str) -> String {
    repo_meta::pattern(pattern)
        .captures(content)
        .unwrap_or_else(|| panic!("no match for {pattern}"))[1]
        .to_owned()
}

/// The image's exact Rust release: the `dhi.io/rust` tag and the `RUST_VERSION` argument the musl
/// standard library is fetched for are one value.
#[test]
fn image_rust_release_is_one_value() {
    let sources = image_release_sources(&meta::repo().read(DOCKERFILE));
    let problems = one_value("the image's Rust release", &sources, &[]);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control of the image's release: a tag and a `RUST_VERSION` that differ, and a
/// Dockerfile with no `RUST_VERSION`, are refused.
#[test]
fn image_rust_release_control_fails_on_another_release() {
    let dockerfile = meta::repo().read(DOCKERFILE);
    let release = pin_of(&dockerfile, r"(?m)^ARG RUST_VERSION=(\S+)\s*$");
    let check = |content: &str| {
        one_value(
            "the image's Rust release",
            &image_release_sources(content),
            &[],
        )
        .len()
    };
    assert_eq!(check(&dockerfile), 0);
    let argument = format!("ARG RUST_VERSION={release}\n");
    assert_eq!(
        check(&altered(&dockerfile, &argument, "ARG RUST_VERSION=1.0.0\n")),
        1
    );
    assert_eq!(
        check(&altered(
            &dockerfile,
            &format!("dhi.io/rust:{release}-"),
            "dhi.io/rust:1.0.0-"
        )),
        1
    );
    assert_eq!(check(&altered(&dockerfile, &argument, "")), 1);
}

/// The shellcheck CI installs, which must be the
/// release the shellcheck-py hook wraps.
#[test]
fn shellcheck_is_one_version() {
    let repo = meta::repo();
    let problems = one_value(
        "ShellCheck",
        &shellcheck_sources(&repo.read(PRE_COMMIT), &repo.read(SETUP_SHELLCHECK)),
        &[],
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control of the `ShellCheck` pin: a CI copy on another release, and a copy that
/// writes none, are refused; the hook's fourth component is not compared.
#[test]
fn shellcheck_control_fails_on_another_version() {
    let repo = meta::repo();
    let hooks = repo.read(PRE_COMMIT);
    let action = repo.read(SETUP_SHELLCHECK);
    let check =
        |action: &str| one_value("ShellCheck", &shellcheck_sources(&hooks, action), &[]).len();
    assert_eq!(check(&action), 0);
    assert_eq!(
        check(&altered(
            &action,
            "SHELLCHECK_VERSION: v0.11.0",
            "SHELLCHECK_VERSION: v0.9.0"
        )),
        1
    );
    assert_eq!(check("env: {}\n"), 1);
}

/// The flags this repository's gates may not carry: the core set and the Rust set, with the rule that a clippy run denies warnings.
fn vacuous_flags() -> VacuousFlags {
    VacuousFlags::core().with_rust()
}

/// The reviewed exceptions. Each must still match a line, so an entry cannot outlive the code it
/// excuses and quietly cover a new line later.
const ALLOWED: &[AllowedFragment<'static>] = &[];

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and every make
/// fragment, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let repo = meta::repo();
    let fragments = repo.files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let mut files = vec!["Makefile".to_owned(), PRE_COMMIT.to_owned()];
    files.extend(fragments);
    files.extend(github_yaml());
    let problems = vacuous_flags().problems(&repo.read_all(&files), ALLOWED);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found; an exception excuses only its own fragment in its own file, and one that
/// matches nothing is a problem.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    for sample in [
        "\tcargo clippy -- -D warnings || true",
        "\tcargo clippy -- -D warnings ||:",
        "\tcargo clippy -- -D warnings || exit 0",
        "\t-cargo clippy -- -D warnings",
        "\t@-cargo clippy -- -D warnings",
        ".IGNORE: rust-clippy",
        "\t$(MAKE) -i rust-test",
        "\t$(MAKE) --ignore-errors rust-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
        "\tcargo clippy --workspace",
        "\tcargo test --workspace --no-run",
    ] {
        let found = flags.problems(&[("sample".to_owned(), sample.to_owned())], &[]);
        assert_eq!(found.len(), 1, "{sample:?}: {found:#?}");
    }

    let release = ".github/workflows/release.yaml";
    let allowed = [AllowedFragment {
        file: release,
        fragment: "scan || true",
        reason: "a fixture",
    }];
    let files = |text: &str| vec![(release.to_owned(), text.to_owned())];
    assert_eq!(
        flags
            .problems(&files("run: make rust-test || true"), &allowed)
            .len(),
        2,
        "an unrelated || true is allowed in release.yaml, or the unused exception is not reported"
    );
    assert_eq!(flags.problems(&files("run: scan || true"), &allowed), []);
    assert_eq!(
        flags
            .problems(
                &files("run: scan || true; make rust-test || true"),
                &allowed
            )
            .len(),
        1,
        "a second || true on an excused line is not found"
    );
    assert_eq!(
        flags
            .problems(
                &[("ci.yaml".to_owned(), "run: scan || true".to_owned())],
                &allowed
            )
            .len(),
        2,
        "an exception excuses another file"
    );
}
