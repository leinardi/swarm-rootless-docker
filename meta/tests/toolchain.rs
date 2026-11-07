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

//! The repository's tooling pins: one Rust line across rust-toolchain.toml, Cargo.toml and the
//! image, with every CI toolchain step on rust-toolchain.toml's exact release; the image's musl
//! standard libraries for the image's own toolchain; one droast and one `ShellCheck` between the
//! hooks and CI; and no flag that makes a gate pass on nothing. The checks are
//! repo-meta's, with this repository's files and pins; the negative controls run them on altered
//! copies.

use repo_meta::toolchain::{
    PinSource, RustVersion, ToolchainRef, ci_toolchain_problems, image_rust_problems, one_value,
    rust_release, rust_version_problems, shellcheck_problems,
};
use repo_meta::vacuous::{AllowedFragment, VacuousFlags};
use repo_meta::{report, strings, yaml};

const TOOLCHAIN: &str = "rust-toolchain.toml";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const PRE_COMMIT: &str = ".pre-commit-config.yaml";
const SETUP_DROAST: &str = ".github/actions/setup-droast/action.yaml";
const SETUP_SHELLCHECK: &str = ".github/actions/setup-shellcheck/action.yaml";
const CI: &str = ".github/workflows/ci.yaml";

/// The exact release rust-toolchain.toml pins: a channel such as stable or 1.98 is refused.
fn release() -> String {
    rust_release(TOOLCHAIN, &meta::repo().read(TOOLCHAIN))
        .unwrap_or_else(|problem| panic!("{problem}"))
}

fn parse(file: &str, content: &str) -> yaml::Value {
    yaml::parse(file, content).unwrap_or_else(|problem| panic!("{problem}"))
}

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

/// Every YAML file under .github, parsed.
fn github_documents() -> Vec<(String, yaml::Value)> {
    let repo = meta::repo();
    github_yaml()
        .into_iter()
        .map(|file| {
            let document = parse(&file, &repo.read(&file));
            (file, document)
        })
        .collect()
}

/// Go: `TestGoLineIsOneValue`, for the MSRV. The `rust-version` in Cargo.toml is the line
/// (major.minor) of the exact release rust-toolchain.toml pins, or a release of that line.
#[test]
fn rust_version_is_the_toolchain_line() {
    let problems = rust_version_problems(
        "Cargo.toml",
        &meta::repo().read("Cargo.toml"),
        &release(),
        RustVersion::Line,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestGoLineIsOneValue` and `TestActionStepsArePinned`, for CI. Every CI step that sets up
/// Rust installs the exact release rust-toolchain.toml pins, never a channel like stable or a
/// release without its patch, which the line would not see move; and some step does.
#[test]
fn ci_installs_the_toolchain_release() {
    let problems = ci_toolchain_problems(&github_documents(), &release(), ToolchainRef::Input);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Go: `TestGoLineIsOneValue`, for the image. The build stage's `dhi.io/rust` tag is on
/// rust-toolchain.toml's line (the image carries its own patch release), every other
/// `dhi.io/rust` reference is that same release, and the musl standard libraries are written for
/// the image's exact toolchain: the `RUST_VERSION` argument is the release in the tag.
#[test]
fn image_builds_on_the_toolchain_line() {
    let problems = image_rust_problems(
        DOCKERFILE,
        &meta::repo().read(DOCKERFILE),
        &release(),
        RustVersion::Line,
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The negative control of the line rules: an MSRV on another line, a tag and a `RUST_VERSION`
/// that differ, and an image on another line, are each refused; a later patch of the line is
/// not.
#[test]
fn rust_line_rules() {
    let release = release();
    let line = release
        .rsplit_once('.')
        .map_or(release.as_str(), |(line, _)| line);
    assert!(
        rust_version_problems(
            "Cargo.toml",
            &format!("rust-version = \"{line}.9\"\n"),
            &release,
            RustVersion::Line
        )
        .is_empty()
    );
    assert_eq!(
        rust_version_problems(
            "Cargo.toml",
            "rust-version = \"1.0\"\n",
            &release,
            RustVersion::Line
        )
        .len(),
        1
    );
    assert_eq!(
        strings(&rust_version_problems(
            "Cargo.toml",
            "[package]\n",
            &release,
            RustVersion::Line
        )),
        ["Cargo.toml: sets no rust-version"]
    );

    let dockerfile = meta::repo().read(DOCKERFILE);
    let check =
        |content: &str| image_rust_problems(DOCKERFILE, content, &release, RustVersion::Line).len();
    assert_eq!(check(&dockerfile), 0);
    let argument = format!("ARG RUST_VERSION={release}\n");
    let tag = format!("dhi.io/rust:{release}-");
    for (broken, want) in [
        (
            dockerfile.replacen(&argument, "ARG RUST_VERSION=1.0.0\n", 1),
            1,
        ),
        (dockerfile.replacen(&tag, "dhi.io/rust:1.0.0-", 1), 2),
        (
            dockerfile
                .replacen(&argument, "ARG RUST_VERSION=1.0.0\n", 1)
                .replacen(&tag, "dhi.io/rust:1.0.0-", 1),
            1,
        ),
        (
            dockerfile.replacen(&argument, &format!("ARG RUST_VERSION={line}\n"), 1),
            1,
        ),
    ] {
        assert_ne!(broken, dockerfile, "a negative control changed nothing");
        assert_eq!(check(&broken), want, "{broken}");
    }
}

/// The meta crate's Dockerfile test runs the droast CI installs; it must be the release the
/// pre-commit hook builds from source, whose rev Dependabot moves.
#[test]
fn droast_is_one_version() {
    let repo = meta::repo();
    let problems = one_value(
        "droast",
        &droast_sources(&repo.read(PRE_COMMIT), &repo.read(SETUP_DROAST)),
        &[],
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

fn droast_sources(hooks: &str, action: &str) -> [PinSource; 2] {
    [
        PinSource::new(
            PRE_COMMIT,
            hooks,
            r"repo: https://github\.com/immanuwell/dockerfile-roast\s+rev:\s*([\d.]+)",
        ),
        PinSource::new(SETUP_DROAST, action, r#"DROAST_VERSION:\s*["']?([\d.]+)"#),
    ]
}

/// The negative control of the droast pin: a CI copy on another release, and a copy that writes
/// none, are refused.
#[test]
fn droast_pin_rules() {
    let repo = meta::repo();
    let hooks = repo.read(PRE_COMMIT);
    let action = repo.read(SETUP_DROAST);
    let moved = regex::Regex::new(r#"(DROAST_VERSION:\s*["']?)[\d.]+"#)
        .expect("valid pattern")
        .replace(&action, "${1}0.0.1")
        .into_owned();
    assert_ne!(moved, action, "a negative control changed nothing");
    assert_eq!(
        one_value("droast", &droast_sources(&hooks, &moved), &[]).len(),
        1
    );
    assert_eq!(
        one_value("droast", &droast_sources(&hooks, "env: {}\n"), &[]).len(),
        1
    );
}

/// Go: `TestActionPinRules`, the negative control of the step rules: each unpinned form is a
/// problem, another action's `toolchain` input included, since any action's toolchain input
/// is held to the release.
#[test]
fn rust_toolchain_pin_rules() {
    let sample = parse(
        "sample.yaml",
        r"jobs:
  test:
    steps:
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567 # master
        with:
          toolchain: 1.98.0
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: stable
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: 1.98
      - uses: dtolnay/rust-toolchain@0123456789abcdef0123456789abcdef01234567
      - uses: some/other-action@0123456789abcdef0123456789abcdef01234567
        with:
          toolchain: nightly
",
    );
    let problems = ci_toolchain_problems(
        &[("sample.yaml".to_owned(), sample)],
        "1.98.0",
        ToolchainRef::Input,
    );
    assert_eq!(problems.len(), 4, "{problems:#?}");
}

/// droast's required shell-linter bridge runs the shellcheck CI installs, which must be the
/// release the shellcheck-py hook wraps. shellcheck-py adds a fourth component of its own
/// (v0.11.0.1), so only the first three are compared.
#[test]
fn shellcheck_is_one_version() {
    let repo = meta::repo();
    let problems = shellcheck_problems(
        PRE_COMMIT,
        &repo.read(PRE_COMMIT),
        SETUP_SHELLCHECK,
        &repo.read(SETUP_SHELLCHECK),
    );
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The flags this repository's gates may not carry: the core set, the JavaScript set and the
/// Rust set, with the rule that a clippy run denies warnings.
fn vacuous_flags() -> VacuousFlags {
    VacuousFlags::core().with_javascript().with_rust()
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
        r#""test": "vitest run --passWithNoTests""#,
        r#""lint": "eslint src --no-error-on-unmatched-pattern""#,
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
