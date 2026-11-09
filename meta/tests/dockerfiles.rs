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

//! The shipped image: the pinned frontend, the build and runtime stages, this image's own runtime
//! rules (the exact dind base, the last USER, the user rewrite, no apk); the negative controls run
//! the stage and runtime rules on altered copies of the real Dockerfile. The `droast` hook enforces
//! the label schema.

use repo_meta::dockerfile::{self, Stage};
use repo_meta::image::{BasePolicy, stage_problems, syntax_problems};
use repo_meta::{pattern, report};

const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const STAGES: &[&str] = &["build", "runtime"];

fn shipped() -> String {
    meta::repo().read(SHIPPED_DOCKERFILE)
}

/// The stage named `name` of `content`, which must be the only one.
fn stage_of(content: &str, name: &str) -> Stage {
    let stages =
        dockerfile::parse(content).unwrap_or_else(|error| panic!("{SHIPPED_DOCKERFILE}: {error}"));
    dockerfile::stage_named(SHIPPED_DOCKERFILE, &stages, name)
        .cloned()
        .unwrap_or_else(|problem| panic!("{problem}"))
}

/// The shipped Dockerfile's stage named `name`, which must be the only one.
fn stage(name: &str) -> Stage {
    stage_of(&shipped(), name)
}

/// This image's own runtime rules: upstream's rootless Docker by its floating major tag and no
/// digest, the last USER the rootless UID and GID, no apk, and the default UID and GID.
fn local_runtime_problems(runtime: &Stage) -> Vec<String> {
    let mut problems = Vec::new();
    // The canonical repository: check-pinned-bases exempts it, as written, from the digest rule,
    // and a bare docker:<tag> would need the whole docker namespace exempted.
    if !pattern(r"^docker\.io/library/docker:\d+-dind-rootless$").is_match(&runtime.image) {
        problems.push(format!(
            "the runtime stage is FROM {:?}, want docker.io/library/docker:<major>-dind-rootless \
             with no digest",
            runtime.image
        ));
    }
    let mut users = Vec::new();
    for instruction in &runtime.instructions {
        match instruction.keyword.as_str() {
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
    for want in ["ROOTLESS_UID=568", "ROOTLESS_GID=568"] {
        if !runtime
            .instructions
            .iter()
            .any(|instruction| instruction.keyword == "ARG" && instruction.args == want)
        {
            problems.push(format!("the runtime stage lacks ARG {want}"));
        }
    }
    problems
}

/// The `BuildKit` frontend that reads the Dockerfile is a literal docker/dockerfile reference
/// pinned by tag and digest: a bare docker/dockerfile:1.7 is a tag that can be moved under a
/// build, and Dependabot never updates the line.
#[test]
fn syntax_directive_is_pinned() {
    let problems = syntax_problems(SHIPPED_DOCKERFILE, &shipped(), true);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// Exactly two named stages, in order, each from a literal base pinned by tag
/// (`BasePolicy::TagOnly`). Neither is pinned by digest in git: dhi.io republishes its tags to
/// ship security fixes, and the release pins the runtime base's digest per build. A variable
/// base, a missing tag, latest or a digest would leave the base unreviewable or frozen.
#[test]
fn shipped_stages_are_build_and_runtime() {
    let problems = stage_problems(SHIPPED_DOCKERFILE, &shipped(), STAGES, BasePolicy::TagOnly);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The shipped Dockerfile with the first `from` replaced by `to`, which must change it.
fn alter(from: &str, to: &str) -> String {
    let shipped = shipped();
    let altered = shipped.replacen(from, to, 1);
    assert_ne!(altered, shipped, "the alteration {from:?} changed nothing");
    altered
}

/// The negative control of the stage rules, on altered copies of the shipped Dockerfile.
#[test]
fn stage_rules_refuse_altered_dockerfiles() {
    // The stages: a digest on the floating base, a variable base, latest, and a third stage.
    for (altered, want) in [
        (
            alter(
                "FROM docker.io/library/docker:29-dind-rootless AS runtime",
                "FROM docker.io/library/docker:29-dind-rootless@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker.io/library/docker:29-dind-rootless AS runtime",
                "FROM $BASE AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker.io/library/docker:29-dind-rootless AS runtime",
                "FROM docker:latest AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker.io/library/docker:29-dind-rootless AS runtime",
                "FROM docker.io/library/docker:29-dind-rootless AS runtime\nFROM busybox:1.37 AS extra",
            ),
            1,
        ),
    ] {
        let problems = stage_problems(SHIPPED_DOCKERFILE, &altered, STAGES, BasePolicy::TagOnly);
        assert_eq!(problems.len(), want, "{}", report(&problems));
    }
}

/// The posture of the image that ships: upstream's rootless Docker by its floating major tag, no
/// package installed, and the rootless user last.
#[test]
fn runtime_stage_is_the_rootless_dind_base() {
    let problems = local_runtime_problems(&stage("runtime"));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The negative control of the image's own runtime rules, on altered copies of the shipped
/// Dockerfile.
#[test]
fn local_runtime_rules_refuse_altered_dockerfiles() {
    for altered in [
        alter(
            "ENV DOCKER_TLS_CERTDIR=\n",
            "ENV DOCKER_TLS_CERTDIR=\nUSER root\n",
        ),
        alter("USER root\n", "USER root\nRUN apk add --no-cache curl\n"),
        alter(
            "FROM docker.io/library/docker:29-dind-rootless AS runtime",
            "FROM docker.io/library/docker:29-dind AS runtime",
        ),
        // The spelling v0.3.0 and earlier use: the same image, but not the canonical repository.
        alter(
            "FROM docker.io/library/docker:29-dind-rootless AS runtime",
            "FROM docker:29-dind-rootless AS runtime",
        ),
    ] {
        assert_eq!(
            local_runtime_problems(&stage_of(&altered, "runtime")).len(),
            1,
            "{altered}"
        );
    }
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
