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

//! The release: the Docker major lives in the Dockerfile alone, the two workflows that read the
//! published images agree on what they are, and every release script is tested in CI.

use meta::repo;
use regex::Regex;
use serde_yaml_ng::Value;

const RELEASE: &str = ".github/workflows/release.yaml";
const SCHEDULED_SCAN: &str = ".github/workflows/scheduled-scan.yaml";
const CI: &str = ".github/workflows/ci.yaml";
const SCRIPTS: &str = ".github/scripts";
const HARNESS: &str = "tests/integration/harness.rs";

/// The files that derive the Docker major from the Dockerfile of the commit they handle, and so
/// must never name one: a literal would be wrong the day the Dockerfile moves to the next major,
/// or for a base rebuild of a release on the previous one.
const MAJOR_FREE: &[&str] = &[
    RELEASE,
    SCHEDULED_SCAN,
    ".github/scripts/release-plan.sh",
    ".github/scripts/scan-findings.sh",
    "scripts/image-test.sh",
];

/// The settings both workflows read the published images with.
const SHARED_ENV: &[&str] = &[
    "RELEASE_TAG_REGEX",
    "IMAGE_REPO",
    "IMAGE_VARIANTS",
    "PLATFORMS",
    "TRIVY_VERSION",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("invalid pattern {pattern:?}: {error}"))
}

fn major_patterns() -> [Regex; 2] {
    [
        re(r"docker:\d+-dind-rootless"),
        // A floating tag: digits that start a word, so not a version's patch or a shell argument.
        re(r"(?:^|[^\w.$])\d+-uid"),
    ]
}

fn major_hits(content: &str) -> Vec<String> {
    let patterns = major_patterns();
    content
        .lines()
        .filter(|line| patterns.iter().any(|pattern| pattern.is_match(line)))
        .map(|line| line.trim().to_owned())
        .collect()
}

fn workflow(file: &str) -> Value {
    serde_yaml_ng::from_str(&repo::read(file))
        .unwrap_or_else(|error| panic!("parse {file}: {error}"))
}

fn env_value(file: &str, document: &Value, key: &str) -> String {
    document["env"][key]
        .as_str()
        .unwrap_or_else(|| panic!("{file} has no top-level env {key}"))
        .to_owned()
}

/// No release file names a Docker major, as a base reference or as a floating tag.
#[test]
fn release_files_name_no_docker_major() {
    let problems: Vec<String> = MAJOR_FREE
        .iter()
        .flat_map(|file| {
            major_hits(&repo::read(file))
                .into_iter()
                .map(move |line| format!("{file}: {line}"))
        })
        .collect();
    assert!(
        problems.is_empty(),
        "a Docker major is named:\n{}",
        problems.join("\n")
    );
}

/// The negative control: both spellings of a major are found, and the placeholders the files use
/// instead are not.
#[test]
fn docker_major_patterns_are_recognized() {
    for sample in [
        "FROM docker:29-dind-rootless AS runtime",
        "ghcr.io/leinardi/swarm-rootless-docker:29-uid568",
        "tags 30-uid1000",
    ] {
        assert_eq!(major_hits(sample).len(), 1, "{sample:?} is not found");
    }
    for sample in [
        "docker:<major>-dind-rootless",
        "the <docker-major>-uid<UID> tags",
        "$MAJOR-uid${entry%%=*}",
        "v0.1.0-uid568",
    ] {
        assert!(major_hits(sample).is_empty(), "{sample:?} is found");
    }
}

/// The scheduled scan reads the images the release publishes, with its scanner: the same tags,
/// repository, variants, platform and Trivy.
#[test]
fn the_scheduled_scan_reads_what_the_release_publishes() {
    let release = workflow(RELEASE);
    let scan = workflow(SCHEDULED_SCAN);
    let problems: Vec<String> = SHARED_ENV
        .iter()
        .filter_map(|key| {
            let published = env_value(RELEASE, &release, key);
            let scanned = env_value(SCHEDULED_SCAN, &scan, key);
            (published != scanned).then(|| {
                format!("{key} is {published:?} in {RELEASE}, {scanned:?} in {SCHEDULED_SCAN}")
            })
        })
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The integration tests run against one variant, the one they are written for: it must be a
/// variant that ships, and its UID the one the tests assert.
#[test]
fn the_integration_variant_is_the_tested_one() {
    let release = workflow(RELEASE);
    let variants = env_value(RELEASE, &release, "IMAGE_VARIANTS");
    let integration = env_value(RELEASE, &release, "INTEGRATION_VARIANT");
    assert!(
        variants
            .split_whitespace()
            .any(|variant| variant == integration),
        "INTEGRATION_VARIANT {integration:?} is not one of IMAGE_VARIANTS {variants:?}"
    );

    let uid = re(r#"(?m)^pub\(crate\) const UID_TEXT: &str = "(\d+)";$"#)
        .captures(&repo::read(HARNESS))
        .map_or_else(
            || panic!("{HARNESS} has no UID_TEXT"),
            |captures| captures[1].to_owned(),
        );
    assert_eq!(
        integration,
        format!("{uid}:{uid}"),
        "the integration tests assert UID {uid}"
    );
}

/// Every release script has a test file, and ci.yaml's release-scripts job, which ci-result needs,
/// runs each one.
#[test]
fn release_scripts_are_tested_in_ci() {
    let files = repo::files_under(SCRIPTS, &[".sh"]);
    let (tests, scripts): (Vec<&String>, Vec<&String>) =
        files.iter().partition(|file| file.ends_with(".test.sh"));
    assert!(
        !scripts.is_empty(),
        "no scripts under {SCRIPTS}; the check would check nothing"
    );

    let ci = workflow(CI);
    let runs: Vec<&str> = ci["jobs"]["release-scripts"]["steps"]
        .as_sequence()
        .unwrap_or_else(|| panic!("{CI} has no release-scripts job"))
        .iter()
        .filter_map(|step| step["run"].as_str())
        .map(str::trim)
        .collect();

    let mut problems = Vec::new();
    for script in &scripts {
        let test = script.replace(".sh", ".test.sh");
        if !tests.contains(&&test) {
            problems.push(format!("{script} has no {test}"));
        }
    }
    for test in &tests {
        if !runs.contains(&format!("bash {test}").as_str()) {
            problems.push(format!("the release-scripts job does not run bash {test}"));
        }
    }
    let needed = ci["jobs"]["ci-result"]["needs"]
        .as_sequence()
        .is_some_and(|needs| {
            needs
                .iter()
                .any(|need| need.as_str() == Some("release-scripts"))
        });
    if !needed {
        problems.push("ci-result does not need release-scripts".to_owned());
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
