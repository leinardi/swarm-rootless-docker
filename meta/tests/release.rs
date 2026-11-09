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
//! published images agree on what they are.

use regex::Regex;
use repo_meta::yaml::{self, Value};

const RELEASE: &str = ".github/workflows/release.yaml";
const SCHEDULED_SCAN: &str = ".github/workflows/scheduled-scan.yaml";
const CI: &str = ".github/workflows/ci.yaml";
const HARNESS: &str = "tests/integration/harness.rs";
const TRIVY_GATE: &str = "leinardi/gh-composite-actions/trivy-gate";

/// The files that derive the Docker major from the Dockerfile of the commit they handle, and so
/// must never name one: a literal would be wrong the day the Dockerfile moves to the next major,
/// or for a base rebuild of a release on the previous one.
const MAJOR_FREE: &[&str] = &[
    RELEASE,
    SCHEDULED_SCAN,
    CI,
    ".github/scripts/release-plan.sh",
    "scripts/image-test.sh",
];

/// The settings both workflows read the published images with.
const SHARED_ENV: &[&str] = &[
    "RELEASE_TAG_REGEX",
    "IMAGE_REPO",
    "IMAGE_VARIANTS",
    "PLATFORMS",
];

/// The settings CI's image job tests the release's packaging path with: every variant it ships,
/// and the one the integration tests are written for.
const CI_IMAGE_ENV: &[&str] = &["IMAGE_VARIANTS", "INTEGRATION_VARIANT"];

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
    yaml::parse(file, &meta::repo().read(file)).unwrap_or_else(|problem| panic!("{problem}"))
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
            major_hits(&meta::repo().read(file))
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
        .captures(&meta::repo().read(HARNESS))
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

/// CI's image job builds and tests every variant the release ships, through the same packaging
/// path, and runs the integration tests on the same one.
#[test]
fn ci_tests_every_variant_the_release_ships() {
    let release = workflow(RELEASE);
    let ci = workflow(CI);
    let problems: Vec<String> = CI_IMAGE_ENV
        .iter()
        .filter_map(|key| {
            let shipped = env_value(RELEASE, &release, key);
            let tested = ci["jobs"]["image"]["env"][key]
                .as_str()
                .unwrap_or_else(|| panic!("{CI}'s image job has no env {key}"));
            (shipped != tested).then(|| {
                format!("{key} is {shipped:?} in {RELEASE}, {tested:?} in {CI}'s image job")
            })
        })
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The UIDs of `variants` (UID:GID pairs) whose built image no trivy-gate step of `release`
/// scans: each variant's scan reads its platform digest from the `images` step's `uid<UID>`
/// output, and a variant added without one would ship unscanned.
fn unscanned_uids(release: &Value, variants: &str) -> Vec<String> {
    let scanned: Vec<String> = yaml::action_steps(RELEASE, release)
        .into_iter()
        .filter(|step| step.action == TRIVY_GATE)
        .filter_map(|step| step.with.get("image").cloned())
        .collect();
    variants
        .split_whitespace()
        .filter_map(|variant| variant.split_once(':').map(|(uid, _)| uid.to_owned()))
        .filter(|uid| {
            let output = format!("steps.images.outputs.uid{uid} }}}}");
            !scanned.iter().any(|image| image.ends_with(&output))
        })
        .collect()
}

/// Every variant the release builds has a scan step of its own before the tag.
#[test]
fn every_released_variant_is_scanned() {
    let release = workflow(RELEASE);
    let variants = env_value(RELEASE, &release, "IMAGE_VARIANTS");
    let unscanned = unscanned_uids(&release, &variants);
    assert!(
        unscanned.is_empty(),
        "no trivy-gate step in {RELEASE} scans the variants with UID {unscanned:?}"
    );
}

/// The negative control: a variant with no scan step is found, and the shipped one is not.
#[test]
fn an_unscanned_variant_is_recognized() {
    let release = workflow(RELEASE);
    let variants = env_value(RELEASE, &release, "IMAGE_VARIANTS");
    assert_eq!(
        unscanned_uids(&release, &format!("{variants} 1000:1000")),
        ["1000"]
    );
}

const DOCKERFILE: &str = "deployments/docker/Dockerfile";
const CHECK_PINNED_BASES: &str = "leinardi/gh-composite-actions/check-pinned-bases";

/// The repository the shipped Dockerfile's runtime stage is FROM, as written.
fn runtime_repository(dockerfile: &str) -> String {
    let from = re(r"(?m)^FROM\s+(\S+)\s+AS\s+runtime\s*$")
        .captures(dockerfile)
        .unwrap_or_else(|| panic!("{DOCKERFILE} has no FROM <image> AS runtime"))[1]
        .to_owned();
    from.rsplit_once(':')
        .map_or(from.clone(), |(repository, _)| repository.to_owned())
}

/// What is wrong with the tag-only exemptions `workflow` passes to check-pinned-bases: exactly
/// the dhi.io registry and `want`, the runtime base's repository as the Dockerfile writes it, so
/// nothing else on Docker Hub may float by tag.
fn tag_only_problems(file: &str, workflow: &Value, want: &str) -> Vec<String> {
    let steps: Vec<_> = yaml::action_steps(file, workflow)
        .into_iter()
        .filter(|step| step.action == CHECK_PINNED_BASES)
        .collect();
    if steps.is_empty() {
        return vec![format!("{file} has no check-pinned-bases step")];
    }
    steps
        .iter()
        .filter_map(|step| {
            let mut entries: Vec<&str> = step
                .with
                .get("tag-only-images")
                .map(|value| {
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            entries.sort_unstable();
            let mut expected = vec!["dhi.io", want];
            expected.sort_unstable();
            (entries != expected).then(|| {
                format!("{file}: check-pinned-bases exempts {entries:?}, want {expected:?}")
            })
        })
        .collect()
}

/// CI exempts exactly the shipped runtime base's canonical repository from the digest rule, and
/// the release the repository of the commit it builds, which the plan resolves (`base_repo`).
#[test]
fn the_tag_only_exemption_is_the_runtime_base() {
    let repository = runtime_repository(&meta::repo().read(DOCKERFILE));
    assert_eq!(repository, "docker.io/library/docker");
    let mut problems = tag_only_problems(CI, &workflow(CI), &repository);
    problems.extend(tag_only_problems(
        RELEASE,
        &workflow(RELEASE),
        "${{ needs.plan.outputs.base_repo }}",
    ));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The negative control: the bare docker namespace, or a missing entry, is refused.
#[test]
fn a_wider_tag_only_exemption_is_recognized() {
    let ci = meta::repo().read(CI);
    for (from, to) in [
        (
            "            docker.io/library/docker\n",
            "            docker.io/library/docker\n            docker\n",
        ),
        (
            "            docker.io/library/docker\n",
            "            docker\n",
        ),
        ("            docker.io/library/docker\n", ""),
    ] {
        assert!(ci.contains(from), "{from:?} is gone from {CI}");
        let altered = yaml::parse(CI, &ci.replacen(from, to, 1))
            .unwrap_or_else(|problem| panic!("{problem}"));
        assert_eq!(
            tag_only_problems(CI, &altered, "docker.io/library/docker").len(),
            1,
            "{to:?}"
        );
    }
}

/// The musl target `make image-binary` builds for (`MUSL_TARGET`) is the one the release adds to
/// the active toolchain in every mode, the one a recovered toolchain installs, and the one the
/// legacy fallback builds: a commit whose rust-toolchain.toml lists no musl target (v0.3.0 and
/// earlier) still builds.
#[test]
fn the_release_installs_the_musl_target_it_builds() {
    let repo = meta::repo();
    let target = repo_meta::toolchain::make_value(&repo.read(".mk/image.mk"), "MUSL_TARGET")
        .expect(".mk/image.mk sets MUSL_TARGET");
    let release_text = repo.read(RELEASE);
    let release = workflow(RELEASE);
    let add = format!("rustup target add {target}");
    assert!(
        release_text.lines().any(|line| line.trim() == add),
        "{RELEASE} never runs `{add}`"
    );
    let recover: Vec<String> = yaml::action_steps(RELEASE, &release)
        .into_iter()
        .filter(|step| step.action == "leinardi/gh-composite-actions/toolchain-recover")
        .map(|step| step.with.get("rust-targets").cloned().unwrap_or_default())
        .collect();
    assert_eq!(
        recover,
        std::slice::from_ref(&target),
        "toolchain-recover's rust-targets"
    );
    assert!(
        release_text.contains(&format!("--target {target} --bin swarm-rootless-docker")),
        "the legacy fallback does not build for {target}"
    );
    let step_order: Vec<String> = yaml::jobs(&release)["release"]
        .steps
        .iter()
        .filter_map(|step| step["name"].as_str().map(str::to_owned))
        .collect();
    let position = |name: &str| {
        step_order
            .iter()
            .position(|step| step == name)
            .unwrap_or_else(|| panic!("the release job has no step {name:?}"))
    };
    assert!(
        position("Recover the recorded toolchain") < position("Add the musl target")
            && position("Add the musl target") < position("Build the launcher binary"),
        "the musl target is not added after the toolchain recovery and before the build"
    );
}
