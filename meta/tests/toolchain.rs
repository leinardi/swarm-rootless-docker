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

//! The repository's tooling pins: one Rust line across rust-toolchain.toml, Cargo.toml, the image
//! and every CI toolchain step; the image's toolchain patch and its musl standard libraries; one
//! droast and one shellcheck between the hooks and CI; and no flag that makes a gate pass on
//! nothing.

use std::collections::BTreeMap;

use meta::repo;
use regex::Regex;
use serde_yaml_ng::Value;

const RUST_TOOLCHAIN_ACTION: &str = "dtolnay/rust-toolchain";
const DOCKERFILE: &str = "deployments/docker/Dockerfile";

/// Rustdoc warnings must fail local verification and the required Rust CI job.
fn rustdoc_gate_present(fragment: &str, workflow: &str) -> bool {
    let verify = fragment.lines().any(|line| {
        line.starts_with("verify-rust:")
            && line.split_whitespace().any(|target| target == "rust-doc")
    });
    let recipe = fragment.contains(
        "rust-doc: ## Check Rust documentation, warnings as errors\n\tRUSTDOCFLAGS=\"$(RUSTDOCFLAGS) -D warnings\" $(CARGO) doc --workspace --all-features --no-deps --locked\n",
    );
    let workflow: Value = serde_yaml_ng::from_str(workflow)
        .unwrap_or_else(|error| panic!("parse the CI workflow: {error}"));
    let ci = workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("rust"))
        .and_then(|job| job.get("steps"))
        .and_then(Value::as_sequence)
        .is_some_and(|steps| {
            steps.iter().any(|step| {
                step.get("run").and_then(Value::as_str) == Some("make rust-doc")
                    && step.get("if").is_none()
                    && step.get("continue-on-error").is_none()
            })
        });
    verify && recipe && ci
}

#[test]
fn rust_documentation_is_a_required_gate() {
    assert!(rustdoc_gate_present(
        &repo::read(".mk/rust.mk"),
        &repo::read(".github/workflows/ci.yaml"),
    ));
}

/// Negative controls reject gates missing from verify or CI, or allowing warnings.
#[test]
fn weakened_rustdoc_gates_are_refused() {
    let fragment = repo::read(".mk/rust.mk");
    let workflow = repo::read(".github/workflows/ci.yaml");
    for broken in [
        fragment.replace("rust-test rust-doc", "rust-test"),
        fragment.replace(" -D warnings", ""),
    ] {
        assert!(!rustdoc_gate_present(&broken, &workflow));
    }
    for broken in [
        workflow.replace("run: make rust-doc", "run: echo missing documentation gate"),
        workflow.replace(
            "run: make rust-doc",
            "if: false\n        run: make rust-doc",
        ),
        workflow.replace(
            "run: make rust-doc",
            "continue-on-error: true\n        run: make rust-doc",
        ),
    ] {
        assert!(!rustdoc_gate_present(&fragment, &broken));
    }
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern}: {error}"))
}

/// One place a version is written: a file and the pattern whose first group is the version there.
struct PinSource {
    file: String,
    pattern: Regex,
}

fn source(file: &str, pattern: &str) -> PinSource {
    PinSource {
        file: file.to_owned(),
        pattern: re(pattern),
    }
}

/// Fails unless every source writes the pin and all of them agree.
fn expect_one_value(what: &str, sources: &[PinSource]) {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut problems = Vec::new();
    for pin in sources {
        let content = repo::read(&pin.file);
        let mut matched = false;
        for captures in pin.pattern.captures_iter(&content) {
            matched = true;
            found
                .entry(captures[1].to_owned())
                .or_default()
                .push(pin.file.clone());
        }
        if !matched {
            problems.push(format!(
                "{what}: {} does not write it ({})",
                pin.file, pin.pattern
            ));
        }
    }
    if found.len() > 1 {
        let values: Vec<String> = found
            .iter()
            .map(|(value, files)| {
                let mut files = files.clone();
                files.dedup();
                format!("{value} in {}", files.join(", "))
            })
            .collect();
        problems.push(format!("{what} is not one value: {}", values.join("; ")));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// One step, in a workflow or a composite action, that uses an action.
#[derive(Debug)]
struct ActionStep {
    file: String,
    /// The `uses:` value without its `@ref`.
    action: String,
    with: BTreeMap<String, String>,
}

/// Every mapping with a `uses:` key anywhere in the document: job steps and composite action
/// steps alike.
fn parse_action_steps(file: &str, content: &str) -> Vec<ActionStep> {
    fn walk(file: &str, node: &Value, steps: &mut Vec<ActionStep>) {
        match node {
            Value::Mapping(mapping) => {
                if let Some(Value::String(uses)) = mapping.get("uses") {
                    let action = uses
                        .split_once('@')
                        .map_or(uses.as_str(), |(action, _)| action);
                    let with = mapping
                        .get("with")
                        .and_then(Value::as_mapping)
                        .map(|inputs| {
                            inputs
                                .iter()
                                .filter_map(|(key, value)| {
                                    let value = match value {
                                        Value::String(text) => text.clone(),
                                        Value::Number(number) => number.to_string(),
                                        Value::Bool(flag) => flag.to_string(),
                                        Value::Null
                                        | Value::Sequence(_)
                                        | Value::Mapping(_)
                                        | Value::Tagged(_) => return None,
                                    };
                                    Some((key.as_str()?.to_owned(), value))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    steps.push(ActionStep {
                        file: file.to_owned(),
                        action: action.to_owned(),
                        with,
                    });
                }
                for value in mapping.values() {
                    walk(file, value, steps);
                }
            }
            Value::Sequence(sequence) => {
                for value in sequence {
                    walk(file, value, steps);
                }
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Tagged(_) => {}
        }
    }

    let document: Value =
        serde_yaml_ng::from_str(content).unwrap_or_else(|error| panic!("parse {file}: {error}"));
    let mut steps = Vec::new();
    walk(file, &document, &mut steps);
    steps
}

/// The Rust step rules: a toolchain step names an exact version, never a channel like stable,
/// which the Rust line test would not see move.
fn rust_pin_problems(steps: &[ActionStep]) -> Vec<String> {
    let exact = re(r"^\d+\.\d+\.\d+$");
    steps
        .iter()
        .filter(|step| step.action == RUST_TOOLCHAIN_ACTION)
        .filter_map(|step| match step.with.get("toolchain") {
            Some(toolchain) if exact.is_match(toolchain) => None,
            Some(toolchain) => Some(format!(
                "{}: {} installs {toolchain:?}; name the exact version of rust-toolchain.toml",
                step.file, step.action
            )),
            None => Some(format!(
                "{}: {} names no toolchain; name the exact version of rust-toolchain.toml",
                step.file, step.action
            )),
        })
        .collect()
}

fn github_yaml() -> Vec<String> {
    let files = repo::files_under(".github", &[".yaml", ".yml"]);
    assert!(
        !files.is_empty(),
        "no YAML files under .github; the scan would check nothing"
    );
    files
}

fn github_steps() -> Vec<ActionStep> {
    github_yaml()
        .iter()
        .flat_map(|file| parse_action_steps(file, &repo::read(file)))
        .collect()
}

/// Go: `TestGoLineIsOneValue`. The Rust line (major.minor) is one value across rust-toolchain.toml,
/// the MSRV in Cargo.toml, the build stage's image tag and every CI toolchain step. The patch may
/// differ: the image carries its own patch release, and the MSRV names no patch at all.
#[test]
fn rust_line_is_one_value() {
    let mut sources = vec![
        source(
            "rust-toolchain.toml",
            r#"(?m)^channel = "(\d+\.\d+)(?:\.\d+)?"$"#,
        ),
        source(
            "Cargo.toml",
            r#"(?m)^rust-version = "(\d+\.\d+)(?:\.\d+)?"$"#,
        ),
        source(DOCKERFILE, r"dhi\.io/rust:(\d+\.\d+)[.-]"),
    ];
    let mut steps = 0;
    for file in github_yaml() {
        if parse_action_steps(&file, &repo::read(&file))
            .iter()
            .any(|step| step.action == RUST_TOOLCHAIN_ACTION)
        {
            steps += 1;
            sources.push(source(
                &file,
                r"(?m)^\s*toolchain:\s*(\d+\.\d+)(?:\.\d+)?\s*$",
            ));
        }
    }
    assert!(
        steps > 0,
        "no {RUST_TOOLCHAIN_ACTION} step found under .github"
    );
    expect_one_value("Rust line", &sources);
}

/// The build stage's standard libraries are written for the image's exact toolchain: the
/// `RUST_VERSION` argument is the version in the image tag.
#[test]
fn image_toolchain_patch_is_the_standard_library_version() {
    let dockerfile = repo::read(DOCKERFILE);
    let tag = re(r"dhi\.io/rust:(\d+\.\d+\.\d+)-")
        .captures(&dockerfile)
        .map(|captures| captures[1].to_owned());
    let argument = re(r"(?m)^ARG RUST_VERSION=(\S+)$")
        .captures(&dockerfile)
        .map(|captures| captures[1].to_owned());
    assert!(
        tag.is_some(),
        "the build stage's tag names no exact Rust version"
    );
    assert_eq!(
        tag, argument,
        "the image's Rust and the RUST_VERSION of its musl standard libraries differ"
    );
}

/// The meta crate's Dockerfile test runs the droast CI installs; it must be the release the
/// pre-commit hook builds from source, whose rev Dependabot moves.
#[test]
fn droast_is_one_version() {
    expect_one_value(
        "droast",
        &[
            source(
                ".pre-commit-config.yaml",
                r"repo: https://github\.com/immanuwell/dockerfile-roast\s+rev:\s*([\d.]+)",
            ),
            source(
                ".github/actions/setup-droast/action.yaml",
                r#"DROAST_VERSION:\s*["']?([\d.]+)"#,
            ),
        ],
    );
}

/// Go: `TestActionStepsArePinned`. Every Rust toolchain step under .github names an exact version.
#[test]
fn rust_toolchain_steps_are_pinned() {
    let steps = github_steps();
    let problems = rust_pin_problems(&steps);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        steps
            .iter()
            .any(|step| step.action == RUST_TOOLCHAIN_ACTION),
        "no {RUST_TOOLCHAIN_ACTION} step found under .github; the rule would check nothing"
    );
}

/// Go: `TestActionPinRules`, the negative control of the step rules: each unpinned form is a
/// problem.
#[test]
fn rust_toolchain_pin_rules() {
    let steps = parse_action_steps(
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
    assert_eq!(steps.len(), 5, "{steps:?}");
    let problems = rust_pin_problems(&steps);
    assert_eq!(problems.len(), 3, "{problems:#?}");
}

/// droast's required shell-linter bridge runs the shellcheck CI installs, which must be the
/// release the shellcheck-py hook wraps. shellcheck-py adds a fourth component of its own
/// (v0.11.0.1), so only the first three are compared.
#[test]
fn shellcheck_is_one_version() {
    expect_one_value(
        "shellcheck",
        &[
            source(
                ".pre-commit-config.yaml",
                r"repo: https://github\.com/shellcheck-py/shellcheck-py\s+rev:\s*(v\d+\.\d+\.\d+)",
            ),
            source(
                ".github/actions/setup-shellcheck/action.yaml",
                r#"SHELLCHECK_VERSION:\s*["']?(v\d+\.\d+\.\d+)"#,
            ),
        ],
    );
}

/// A flag that turns a gate with nothing to check, or a failing one, into a pass.
struct VacuousFlag {
    name: &'static str,
    pattern: Regex,
}

fn vacuous_flags() -> Vec<VacuousFlag> {
    [
        ("--passWithNoTests", r"--pass-?[Ww]ith-?[Nn]o-?[Tt]ests"),
        (
            "--no-error-on-unmatched-pattern",
            r"--no-error-on-unmatched-pattern",
        ),
        ("|| true", r"\|\|\s*(true\b|:(\s|;|$)|exit\s+0\b)"),
        ("make's - recipe prefix", r"^\t[@+]*-"),
        (".IGNORE", r"^\.IGNORE\b"),
        (
            "make --ignore-errors",
            r"\$\(MAKE\).*\s(-[a-zA-Z]*i[a-zA-Z]*|--ignore-errors)\b",
        ),
        ("continue-on-error", r"continue-on-error"),
        (
            "if: false",
            r#"\bif:\s*(["']?false["']?\s*$|["']?\$\{\{\s*false\s*\}\}["']?)"#,
        ),
    ]
    .into_iter()
    .map(|(name, pattern)| VacuousFlag {
        name,
        pattern: re(pattern),
    })
    .collect()
}

/// A vacuous-looking line that is not one: the file it may appear in, and the pattern the line
/// must match.
struct AllowedHit {
    file: &'static str,
    pattern: Regex,
    reason: &'static str,
}

/// The reviewed exceptions. Each must still match at least one line, so an entry cannot outlive
/// the code it excuses and quietly cover a new line later.
fn allowed_hits() -> Vec<AllowedHit> {
    Vec::new()
}

struct VacuousHit {
    number: usize,
    flag: &'static str,
    line: String,
}

fn vacuous_hits(flags: &[VacuousFlag], content: &str) -> Vec<VacuousHit> {
    content
        .split('\n')
        .enumerate()
        .flat_map(|(index, line)| {
            flags
                .iter()
                .filter(|flag| flag.pattern.is_match(line))
                .map(move |flag| VacuousHit {
                    number: index + 1,
                    flag: flag.name,
                    line: line.trim().to_owned(),
                })
        })
        .collect()
}

/// The index of the allowed hit that excuses `line` in `file`. The excused construct must be the
/// line's only flag: an `|| true` elsewhere on the same line stays a finding.
fn allowed_index(
    flags: &[VacuousFlag],
    allowed: &[AllowedHit],
    file: &str,
    line: &str,
) -> Option<usize> {
    allowed.iter().position(|entry| {
        entry.file == file
            && entry.pattern.is_match(line)
            && vacuous_hits(flags, &entry.pattern.replace_all(line, "")).is_empty()
    })
}

/// Go: `TestNoVacuousFlags`. Scans the files that define gates: the Makefile and every make
/// fragment, the pre-commit configuration and every YAML file under .github, the workflows
/// included.
#[test]
fn no_vacuous_flags() {
    let flags = vacuous_flags();
    let allowed = allowed_hits();
    let fragments = repo::files_under(".mk", &[".mk"]);
    assert!(
        !fragments.is_empty(),
        "no make fragments found under .mk/; the scan would check nothing"
    );
    let mut files = vec!["Makefile".to_owned(), ".pre-commit-config.yaml".to_owned()];
    files.extend(fragments);
    files.extend(github_yaml());

    let mut used = vec![false; allowed.len()];
    let mut problems = Vec::new();
    for file in &files {
        for hit in vacuous_hits(&flags, &repo::read(file)) {
            match allowed_index(&flags, &allowed, file, &hit.line) {
                Some(index) => used[index] = true,
                None => problems.push(format!("{file}:{}: {}: {}", hit.number, hit.flag, hit.line)),
            }
        }
    }
    for (entry, used) in allowed.iter().zip(used) {
        if !used {
            problems.push(format!(
                "the allowed hit {:?} in {} matches no line; remove it",
                entry.reason, entry.file
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Go: `TestVacuousFlagsAreRecognized`, the negative control: each flag, written as a gate would
/// carry it, is found, and an allowed pattern excuses only its own file.
#[test]
fn vacuous_flags_are_recognized() {
    let flags = vacuous_flags();
    let allowed = allowed_hits();
    for sample in [
        r#""test": "vitest run --passWithNoTests""#,
        r#""lint": "eslint src --no-error-on-unmatched-pattern""#,
        "\tcargo clippy || true",
        "\tcargo clippy ||:",
        "\tcargo clippy || exit 0",
        "\t-cargo clippy",
        "\t@-cargo clippy",
        ".IGNORE: rust-clippy",
        "\t$(MAKE) -i rust-test",
        "\t$(MAKE) --ignore-errors rust-test",
        "        continue-on-error: true",
        "    if: false",
        "    if: ${{ false }}",
        "    if: 'false'",
        "    if: \"${{ false }}\"",
    ] {
        assert!(
            !vacuous_hits(&flags, sample).is_empty(),
            "no vacuous flag found in {sample:?}"
        );
    }

    assert!(
        allowed_index(
            &flags,
            &allowed,
            ".github/workflows/release.yaml",
            "make rust-test || true"
        )
        .is_none(),
        "an unrelated || true is allowed in release.yaml"
    );
}
