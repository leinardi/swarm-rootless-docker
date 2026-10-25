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

//! The shipped image: its one Dockerfile, the pinned frontend, the build and runtime stages, the
//! labels, and droast's enforcement of the label schema.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meta::dockerfile::{self, Stage};
use meta::{repo, toolgate};
use regex::Regex;

const SHIPPED_DIR: &str = "deployments/docker";
const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const IMAGE_FRAGMENT: &str = ".mk/image.mk";
const DROAST_CONFIG: &str = "droast.toml";
const BINARY: &str = "/usr/local/bin/swarm-rootless-docker";

/// The labels of the runtime stage; an empty value means any non-empty one. The base name is
/// checked against the stage's own FROM.
const WANT_LABELS: &[(&str, &str)] = &[
    ("org.opencontainers.image.title", "swarm-rootless-docker"),
    ("org.opencontainers.image.description", ""),
    (
        "org.opencontainers.image.source",
        "https://github.com/leinardi/swarm-rootless-docker",
    ),
    ("org.opencontainers.image.version", "${VERSION}"),
    ("org.opencontainers.image.revision", "${COMMIT}"),
    ("org.opencontainers.image.created", "${DATE}"),
    ("org.opencontainers.image.licenses", "MIT"),
    ("org.opencontainers.image.base.name", ""),
    ("org.opencontainers.image.base.digest", "${BASE_DIGEST}"),
    (
        "io.github.leinardi.swarm-rootless-docker.uid",
        "${ROOTLESS_UID}",
    ),
    (
        "io.github.leinardi.swarm-rootless-docker.gid",
        "${ROOTLESS_GID}",
    ),
];

fn shipped_stages() -> Vec<Stage> {
    dockerfile::parse(&repo::read(SHIPPED_DOCKERFILE))
        .unwrap_or_else(|error| panic!("{SHIPPED_DOCKERFILE}: {error}"))
}

fn stage(name: &str) -> Stage {
    let found: Vec<Stage> = shipped_stages()
        .into_iter()
        .filter(|candidate| candidate.name == name)
        .collect();
    match <[Stage; 1]>::try_from(found) {
        Ok([stage]) => stage,
        Err(found) => panic!(
            "{SHIPPED_DOCKERFILE} has {} stages named {name}, want one",
            found.len()
        ),
    }
}

/// deployments/docker holds exactly the image that ships.
/// A second file there would look shipped to every reader and be built by nothing.
#[test]
fn shipped_dockerfile_is_the_only_one() {
    let mut found: Vec<String> = fs::read_dir(repo::path(SHIPPED_DIR))
        .expect("list deployments/docker")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name == "Dockerfile" || name.ends_with(".Dockerfile"))
        .collect();
    found.sort();
    assert_eq!(found, ["Dockerfile"], "{SHIPPED_DIR} holds {found:?}");
}

/// The `BuildKit` frontend that reads the Dockerfile is a
/// literal reference pinned by digest: a bare docker/dockerfile:1.7 is a tag that can be moved
/// under a build, and Dependabot never updates the line.
#[test]
fn syntax_directive_is_pinned() {
    let syntax = dockerfile::syntax(&repo::read(SHIPPED_DOCKERFILE))
        .expect("the Dockerfile has a syntax parser directive on its first line");
    assert_eq!(
        dockerfile::repository(&syntax),
        "docker/dockerfile",
        "{syntax}"
    );
    assert!(
        dockerfile::digest_pinned(&syntax),
        "the frontend {syntax:?} is not pinned by digest"
    );
}

/// Exactly two named stages, in order, each from a literal base pinned by tag. Neither is pinned
/// by digest in git: dhi.io republishes its tags to ship security fixes, and the release pins the
/// runtime base's digest per build. A variable base, a missing tag or latest would leave the base
/// unreviewable.
#[test]
fn shipped_stages_are_build_and_runtime() {
    let stages = shipped_stages();
    for stage in &stages {
        assert!(
            dockerfile::tag_pinned(&stage.image),
            "stage {:?} is FROM {:?}, want a literal reference with a tag other than latest",
            stage.name,
            stage.image
        );
    }
    let names: Vec<&str> = stages.iter().map(|stage| stage.name.as_str()).collect();
    assert_eq!(names, ["build", "runtime"]);
}

/// The Rust build: on the build platform, cross-compiled by
/// cargo-zigbuild to the target platform's static musl triple, from the locked dependency graph,
/// with the toolchain downloads checked, and stamped with the version strings the binary reads.
#[test]
fn build_stage() {
    let build = stage("build");
    assert_eq!(build.platform, "$BUILDPLATFORM");
    assert_eq!(dockerfile::repository(&build.image), "dhi.io/rust");

    let instructions = build.joined();
    let missing: Vec<&str> = [
        "ARG VERSION",
        "ARG COMMIT",
        "ARG DATE",
        "ARG RUST_VERSION=",
        "ARG ZIG_VERSION=",
        "ARG CARGO_ZIGBUILD_VERSION=",
        "ARG BUILDARCH",
        "ARG TARGETARCH",
        "amd64) triple=x86_64-unknown-linux-musl;",
        "arm64) triple=aarch64-unknown-linux-musl;",
        "sha256sum -c -",
        r#"cargo install --locked "cargo-zigbuild@${CARGO_ZIGBUILD_VERSION}""#,
        r#"cargo zigbuild --release --locked --target "${triple}" --bin swarm-rootless-docker"#,
        r#"SWARM_ROOTLESS_DOCKER_VERSION="${VERSION}""#,
        r#"SWARM_ROOTLESS_DOCKER_COMMIT="${COMMIT}""#,
        r#"SWARM_ROOTLESS_DOCKER_DATE="${DATE}""#,
        r#"cp "target/${triple}/release/swarm-rootless-docker" /out/swarm-rootless-docker"#,
    ]
    .into_iter()
    .filter(|want| !instructions.contains(want))
    .collect();
    assert!(
        missing.is_empty(),
        "the build stage lacks {missing:?}:\n{instructions}"
    );

    // Every download is checked against its own digest: each curl in the stage is followed by a
    // sha256sum check of the file it wrote.
    let downloads = instructions.matches("curl -fsSL").count();
    let checks = instructions.matches("| sha256sum -c -").count();
    assert!(downloads > 0, "the build stage downloads nothing");
    assert_eq!(
        downloads, checks,
        "a download in the build stage is not checked"
    );
}

/// The posture of the image that ships: upstream's rootless Docker by its floating major tag, the
/// binary as the entrypoint and the healthcheck, a non-root default user and no TLS directory. No
/// package is installed and no volume or port is declared: the image adds the launcher to
/// upstream's and nothing else.
#[test]
fn runtime_stage() {
    let runtime = stage("runtime");
    assert!(
        Regex::new(r"^docker:\d+-dind-rootless$")
            .expect("valid pattern")
            .is_match(&runtime.image),
        "the runtime stage is FROM {:?}, want docker:<major>-dind-rootless with no digest",
        runtime.image
    );
    assert!(
        runtime.platform.is_empty(),
        "the runtime stage pins --platform={}; it has to follow the target platform",
        runtime.platform
    );

    let mut problems = Vec::new();
    let mut users = Vec::new();
    for instruction in &runtime.instructions {
        match instruction.keyword.as_str() {
            "VOLUME" | "ADD" | "EXPOSE" | "CMD" | "ONBUILD" => problems.push(format!(
                "the runtime stage carries {} {}",
                instruction.keyword, instruction.args
            )),
            "USER" => users.push(instruction.args.clone()),
            "RUN" if instruction.args.contains("apk") => problems.push(format!(
                "the runtime stage installs packages: {}",
                instruction.args
            )),
            _ => {}
        }
    }
    if users.last().map(String::as_str) != Some("${ROOTLESS_UID}:${ROOTLESS_GID}") {
        problems.push(format!(
            "the runtime stage's users are {users:?}; the last must be the rootless UID and GID"
        ));
    }
    for (keyword, args) in [
        ("ENTRYPOINT", format!(r#"["{BINARY}"]"#)),
        (
            "COPY",
            format!("--from=build /out/swarm-rootless-docker {BINARY}"),
        ),
        ("ENV", "DOCKER_TLS_CERTDIR=".to_owned()),
        (
            "HEALTHCHECK",
            format!(
                r#"--interval=10s --timeout=5s --start-period=120s --retries=3 CMD ["{BINARY}", "healthcheck"]"#
            ),
        ),
    ] {
        let seen: Vec<&str> = runtime
            .instructions
            .iter()
            .filter(|instruction| instruction.keyword == keyword)
            .map(|instruction| instruction.args.as_str())
            .collect();
        if seen != [args.as_str()] {
            problems.push(format!(
                "the runtime stage has {keyword} {seen:?}, want exactly {keyword} {args}"
            ));
        }
    }
    for want in ["ROOTLESS_UID=568", "ROOTLESS_GID=568"] {
        if !runtime
            .instructions
            .iter()
            .any(|instruction| instruction.keyword == "ARG" && instruction.args == want)
        {
            problems.push(format!("the runtime stage lacks ARG {want}"));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// One RUN in the runtime stage, and it is the user rewrite: renumber the user and group and their
/// files, refuse root, a non-number and an ID taken by another entry, and empty the subordinate ID
/// files the launcher replaces.
#[test]
fn the_user_rewrite_is_the_only_run() {
    let runtime = stage("runtime");
    let mut problems = Vec::new();
    let runs: Vec<&str> = runtime
        .instructions
        .iter()
        .filter(|instruction| instruction.keyword == "RUN")
        .map(|instruction| instruction.args.as_str())
        .collect();
    if let [rewrite] = runs.as_slice() {
        for want in [
            "/etc/passwd",
            "/etc/group",
            r#"chown -h "${uid}""#,
            r#"chgrp -h "${gid}""#,
            ": > /etc/subuid",
            ": > /etc/subgid",
            "already belongs to another user",
            "already belongs to another group",
            "must be from 1 to 4294967294",
        ] {
            if !rewrite.contains(want) {
                problems.push(format!("the user rewrite lacks {want:?}"));
            }
        }
    } else {
        problems.push(format!(
            "the runtime stage has {} RUN instructions, want exactly the user rewrite",
            runs.len()
        ));
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Exactly the expected labels, the base name naming the stage's own FROM.
#[test]
fn runtime_labels() {
    let runtime = stage("runtime");
    let mut problems = Vec::new();
    let labels = dockerfile::labels(&runtime);
    for (key, value) in WANT_LABELS {
        let actual = labels.get(*key).map_or("", String::as_str);
        if actual.is_empty() || (!value.is_empty() && actual != *value) {
            problems.push(format!("label {key} is {actual:?}, want {value:?}"));
        }
    }
    if labels.len() != WANT_LABELS.len() {
        problems.push(format!(
            "the runtime stage carries labels {labels:?}, want exactly {} labels",
            WANT_LABELS.len()
        ));
    }
    let base_name = labels
        .get("org.opencontainers.image.base.name")
        .map_or("", String::as_str);
    if base_name != format!("docker.io/library/{}", runtime.image) {
        problems.push(format!(
            "the base name label is {base_name:?}, not the stage's FROM {:?}",
            runtime.image
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every build of the image passes the build arguments its labels read: without them the labels
/// say dev, none and unknown, and the UID and GID fall back to the defaults.
#[test]
fn label_arguments_are_supplied() {
    let content = repo::read(IMAGE_FRAGMENT);
    for argument in ["VERSION", "COMMIT", "DATE", "ROOTLESS_UID", "ROOTLESS_GID"] {
        let pattern = Regex::new(&format!(r"--build-arg {argument}=\S")).expect("valid pattern");
        assert!(
            pattern.is_match(&content),
            "{IMAGE_FRAGMENT} does not pass the {argument} build argument"
        );
    }
}

/// The label test, by name: the gate tests below run it again in a child process.
const LABEL_TEST: &str = "droast_enforces_the_labels";

/// The path of droast, after checking that the `shellcheck` its required bridge runs is on `PATH`
/// too. `None` when either is missing and the test should return early.
fn lookup_droast() -> Option<PathBuf> {
    let droast = toolgate::lookup(toolgate::DROAST)?;
    toolgate::lookup(toolgate::SHELLCHECK)?;
    Some(droast)
}

fn run_droast(droast: &Path, dockerfile: &Path) -> Output {
    Command::new(droast)
        .arg("--config")
        .arg(repo::path(DROAST_CONFIG))
        .arg(dockerfile)
        .output()
        .unwrap_or_else(|error| panic!("run droast: {error}"))
}

/// droast, with the repository's configuration, passes the shipped Dockerfile and fails the same
/// file without one of its labels with DF074, a missing required label, which is an error: an OCI
/// label, and the UID label for the image's own.
#[test]
fn droast_enforces_the_labels() {
    let Some(droast) = lookup_droast() else {
        return;
    };

    let output = run_droast(&droast, &repo::path(SHIPPED_DOCKERFILE));
    assert!(
        output.status.success(),
        "droast rejects {SHIPPED_DOCKERFILE}:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let shipped = repo::read(SHIPPED_DOCKERFILE);
    let without_version = Regex::new(r"(?m)^\s*org\.opencontainers\.image\.version=.*\n")
        .expect("valid pattern")
        .replace_all(&shipped, "")
        .into_owned();
    assert_ne!(
        without_version, shipped,
        "could not remove the version label from the fixture"
    );
    let scratch = tempfile::tempdir().expect("scratch directory");
    let scratch_dockerfile = scratch.path().join("Dockerfile");
    fs::write(&scratch_dockerfile, without_version).expect("write the fixture");

    let output = run_droast(&droast, &scratch_dockerfile);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && text.contains("DF074"),
        "droast accepted a Dockerfile without the version label:\n{text}"
    );

    let without_uid = Regex::new(r"(?m)^\s*io\.github\.leinardi\.swarm-rootless-docker\.uid=.*\n")
        .expect("valid pattern")
        .replace_all(&shipped, "")
        .into_owned();
    assert_ne!(
        without_uid, shipped,
        "could not remove the UID label from the fixture"
    );
    fs::write(&scratch_dockerfile, without_uid).expect("write the fixture");
    let output = run_droast(&droast, &scratch_dockerfile);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && text.contains("DF074"),
        "droast accepted a Dockerfile without the UID label:\n{text}"
    );
}

/// Runs the label test alone in a child process whose `PATH` holds only a stub `droast`, so
/// droast is found and the `shellcheck` its bridge needs is not; `require` sets the toolgate
/// variable to 1, otherwise it is unset.
fn run_label_test_without_shellcheck(require: bool) -> Output {
    let path = tempfile::tempdir()
        .unwrap_or_else(|error| panic!("create the scratch PATH directory: {error}"));
    let stub = path.path().join(toolgate::DROAST);
    fs::write(&stub, "#!/bin/sh\nexit 0\n")
        .unwrap_or_else(|error| panic!("write the stub droast: {error}"));
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("make the stub droast executable: {error}"));

    let test_binary =
        std::env::current_exe().unwrap_or_else(|error| panic!("find the test binary: {error}"));
    let mut command = Command::new(test_binary);
    command
        .args([LABEL_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env("PATH", path.path());
    if require {
        command.env(toolgate::REQUIRE_ENV, "1");
    } else {
        command.env_remove(toolgate::REQUIRE_ENV);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("run {LABEL_TEST} in a child process: {error}"))
}

/// The label test with droast but no shellcheck, outside CI: it returns early, passing, and says
/// that it did not run and why.
#[test]
fn label_test_skips_without_shellcheck_by_default() {
    let output = run_label_test_without_shellcheck(false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{LABEL_TEST} did not pass without shellcheck:\n{stdout}{stderr}"
    );
    assert!(
        stderr.contains("shellcheck not found on PATH"),
        "{LABEL_TEST} skipped without naming the missing shellcheck:\n{stderr}"
    );
}

/// The label test with droast but no shellcheck, as CI runs it: a failure, not a skip.
#[test]
fn label_test_fails_without_shellcheck_when_tools_are_required() {
    let output = run_label_test_without_shellcheck(true);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stdout.contains("1 failed"),
        "{LABEL_TEST} did not fail without shellcheck under {}=1:\n{stdout}{stderr}",
        toolgate::REQUIRE_ENV
    );
    assert!(
        format!("{stdout}{stderr}").contains("shellcheck is required"),
        "{LABEL_TEST} failed for another reason than the missing shellcheck:\n{stdout}{stderr}"
    );
}

/// The negative control for the reader: each
/// of these inputs has to come out as something the assertions above refuse.
#[test]
fn parse_rejects_what_the_assertions_must_not_miss() {
    const DIGEST: &str = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    for (name, image) in [
        ("no tag", "dhi.io/static"),
        ("latest", "dhi.io/static:latest"),
        ("a variable tag", "dhi.io/rust:${RUST_LINE}-debian13-dev"),
        ("a variable image", "$BASE"),
        ("a stage name, not a ref", "build"),
    ] {
        assert!(
            !dockerfile::tag_pinned(image),
            "{name} ({image:?}) passes as tag-pinned"
        );
    }
    assert!(dockerfile::tag_pinned("dhi.io/static:20250419"));

    for (name, image) in [
        ("a tag only", "docker/dockerfile:1.7".to_owned()),
        (
            "a short digest",
            "docker/dockerfile:1.7@sha256:0123".to_owned(),
        ),
        ("a digest with no tag", format!("docker/dockerfile{DIGEST}")),
    ] {
        assert!(
            !dockerfile::digest_pinned(&image),
            "{name} ({image:?}) passes as digest-pinned"
        );
    }
    assert!(dockerfile::digest_pinned(&format!(
        "docker/dockerfile:1.7{DIGEST}"
    )));

    let stages =
        dockerfile::parse("FROM a:1 AS one\nRUN echo \\\n  two\n# note\nFROM b:2\nUSER 1:1\n")
            .expect("parse");
    assert_eq!(stages.len(), 2, "{stages:?}");
    assert_eq!(stages[0].name, "one");
    assert_eq!(stages[1].name, "");
    assert_eq!(stages[0].joined(), "RUN echo two");
    assert_eq!(stages[1].joined(), "USER 1:1");

    for (name, content) in [
        ("no directive", "FROM a:1\n".to_owned()),
        (
            "after a comment",
            format!("# note\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
        (
            "after a blank line",
            format!("\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
        (
            "after an instruction",
            format!("FROM a:1\n# syntax=docker/dockerfile:1.7{DIGEST}\n"),
        ),
    ] {
        assert_eq!(
            dockerfile::syntax(&content),
            None,
            "{name}: read a syntax directive BuildKit ignores"
        );
    }

    assert!(
        dockerfile::parse("FROM a:1\nRUN echo \\\n").is_err(),
        "a file ending inside a continuation parsed"
    );
}
