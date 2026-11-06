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
//! version stamps, the labels, and droast's enforcement of the label schema. The generic checks
//! are `repo_meta::image`'s, with this repository's parameters, beside the rules that are this
//! image's own (the exact dind base, the last USER, the user rewrite, no apk, the `base.name`
//! label, the label build arguments, and droast); the negative controls run them on altered
//! copies of the real Dockerfile.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regex::Regex;
use repo_meta::dockerfile::{self, Stage};
use repo_meta::image::{
    BasePolicy, BuildBase, BuildStage, Download, RuntimeStage, STAMP_FIELDS, StampSources, Stamps,
    build_stage_problems, dockerfiles, label_problems, only_dockerfile_problems, runtime_problems,
    stage_problems, syntax_problems,
};
use repo_meta::{Problem, pattern, report, strings};

const SHIPPED_DOCKERFILE: &str = "deployments/docker/Dockerfile";
const IMAGE_FRAGMENT: &str = ".mk/image.mk";
const DROAST_CONFIG: &str = "droast.toml";
const BINARY: &str = "/usr/local/bin/swarm-rootless-docker";
const STAGES: &[&str] = &["build", "runtime"];
const BASE_NAME_LABEL: &str = "org.opencontainers.image.base.name";

const STAMPS: Stamps<'static> = Stamps {
    prefix: "SWARM_ROOTLESS_DOCKER",
    fields: &STAMP_FIELDS,
    version_file: "src/version.rs",
    build_command: "cargo zigbuild",
};

/// The labels of the runtime stage, exactly; `None` means any non-empty value. The base name is
/// checked against the stage's own FROM by [`runtime_labels`].
const WANT_LABELS: &[(&str, Option<&str>)] = &[
    (
        "org.opencontainers.image.title",
        Some("swarm-rootless-docker"),
    ),
    ("org.opencontainers.image.description", None),
    (
        "org.opencontainers.image.source",
        Some("https://github.com/leinardi/swarm-rootless-docker"),
    ),
    ("org.opencontainers.image.version", Some("${VERSION}")),
    ("org.opencontainers.image.revision", Some("${COMMIT}")),
    ("org.opencontainers.image.created", Some("${DATE}")),
    ("org.opencontainers.image.licenses", Some("MIT")),
    (BASE_NAME_LABEL, None),
    (
        "org.opencontainers.image.base.digest",
        Some("${BASE_DIGEST}"),
    ),
    (
        "io.github.leinardi.swarm-rootless-docker.uid",
        Some("${ROOTLESS_UID}"),
    ),
    (
        "io.github.leinardi.swarm-rootless-docker.gid",
        Some("${ROOTLESS_GID}"),
    ),
];

/// What the build stage's instructions must contain: the version arguments, the toolchain
/// releases, the platform arguments and each platform's triple, the sha256 check, the locked
/// cargo-zigbuild install, the locked static musl build of the binary, stamped, and its copy out of
/// the cache.
const BUILD_REQUIRED: &[&str] = &[
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
];

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

/// The first group of `pattern` in `content`, or a placeholder no rule accepts.
fn captured(content: &str, pattern_text: &str) -> String {
    pattern(pattern_text).captures(content).map_or_else(
        || format!("<no match for {pattern_text}>"),
        |captures| captures[1].to_owned(),
    )
}

/// The arms a download must have, each `(platform, asset)` with the sha256 the Dockerfile's arm
/// for exactly that platform and asset writes. The pairs are this repository's; a digest that is
/// not a sha256, or an arm with another asset, leaves the arm unread, so the rule refuses it.
fn arms(
    content: &str,
    asset_var: &str,
    sha_var: &str,
    pairs: &[(&str, &str)],
) -> Vec<(String, String, String)> {
    pairs
        .iter()
        .map(|(platform, asset)| {
            let digest = captured(
                content,
                &format!(
                    r"{}\)\s*{asset_var}={}; {sha_var}=([0-9a-f]{{64}}) ;;",
                    regex::escape(platform),
                    regex::escape(asset)
                ),
            );
            ((*platform).to_owned(), (*asset).to_owned(), digest)
        })
        .collect()
}

/// The build stage's two downloads: zig for the build platform, and the target platform's musl
/// standard library for the image's own Rust release (the `dhi.io/rust` tag's, which the
/// toolchain test holds to `ARG RUST_VERSION` and to rust-toolchain.toml's line); each in its own
/// RUN, chosen by its platform argument's `case` with exactly these arms, and checked with
/// `sha256sum -c -` there. zig's release is the Dockerfile's own `ARG ZIG_VERSION`: no other file
/// pins it.
fn downloads(content: &str) -> Vec<Download<'static>> {
    vec![
        Download {
            tool: "zig",
            url: "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz",
            platform_arg: "BUILDARCH",
            version_arg: "ZIG_VERSION",
            version: captured(content, r"(?m)^ARG ZIG_VERSION=(\S+)$"),
            asset_var: "zig_arch",
            sha_var: "zig_sha256",
            arms: arms(
                content,
                "zig_arch",
                "zig_sha256",
                &[("amd64", "x86_64"), ("arm64", "aarch64")],
            ),
        },
        Download {
            tool: "the musl rust-std",
            url: "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz",
            platform_arg: "TARGETARCH",
            version_arg: "RUST_VERSION",
            version: captured(content, r"(?m)^FROM .*\bdhi\.io/rust:(\d+\.\d+\.\d+)-"),
            asset_var: "triple",
            sha_var: "std_sha256",
            arms: arms(
                content,
                "triple",
                "std_sha256",
                &[
                    ("amd64", "x86_64-unknown-linux-musl"),
                    ("arm64", "aarch64-unknown-linux-musl"),
                ],
            ),
        },
    ]
}

/// The build stage's rules against `content`: on the build platform, from dhi.io/rust,
/// cross-compiled by cargo-zigbuild to the target platform's static musl triple from the locked
/// dependency graph, stamped, with every download a rule's and checked in its RUN.
fn build_problems(content: &str) -> Vec<Problem> {
    let downloads = downloads(content);
    let rules = BuildStage {
        platform: "$BUILDPLATFORM",
        base: BuildBase::Repository("dhi.io/rust"),
        required: BUILD_REQUIRED,
        downloads: &downloads,
    };
    build_stage_problems(&stage_of(content, "build"), &rules)
}

/// The stamps of `content`, against src/version.rs, the Makefile and make-common's rust.mk.
fn stamp_problems(content: &str) -> Vec<Problem> {
    stamp_problems_with(content, &meta::repo().read(STAMPS.version_file))
}

/// [`stamp_problems`], against the version module `version_rs`.
fn stamp_problems_with(content: &str, version_rs: &str) -> Vec<Problem> {
    let repo = meta::repo();
    let build = stage_of(content, "build");
    STAMPS.problems(&StampSources {
        version_rs,
        build: &build,
        makefile: &repo.read("Makefile"),
        rust_mk: &repo.read(".mk/rust.mk"),
        release: None,
    })
}

/// The runtime stage's generic rules: from upstream's `docker` repository on the target platform,
/// no VOLUME, ADD, EXPOSE, CMD or ONBUILD, and exactly one ENTRYPOINT (the binary), one COPY (the
/// binary from the build stage), one ENV (no TLS directory) and one HEALTHCHECK (the binary's
/// healthcheck mode, with its timings).
fn runtime_stage_problems(runtime: &Stage) -> Vec<Problem> {
    let entrypoint = format!(r#"["{BINARY}"]"#);
    let copy = format!("--from=build /out/swarm-rootless-docker {BINARY}");
    let healthcheck = format!(
        r#"--interval=10s --timeout=5s --start-period=120s --retries=3 CMD ["{BINARY}", "healthcheck"]"#
    );
    runtime_problems(
        runtime,
        &RuntimeStage {
            repository: "docker",
            forbidden: &["VOLUME", "ADD", "EXPOSE", "CMD", "ONBUILD"],
            exactly: &[
                ("ENTRYPOINT", &entrypoint),
                ("COPY", &copy),
                ("ENV", "DOCKER_TLS_CERTDIR="),
                ("HEALTHCHECK", &healthcheck),
            ],
        },
    )
}

/// This image's own runtime rules: upstream's rootless Docker by its floating major tag and no
/// digest, the last USER the rootless UID and GID, no apk, and the default UID and GID.
fn local_runtime_problems(runtime: &Stage) -> Vec<String> {
    let mut problems = Vec::new();
    if !pattern(r"^docker:\d+-dind-rootless$").is_match(&runtime.image) {
        problems.push(format!(
            "the runtime stage is FROM {:?}, want docker:<major>-dind-rootless with no digest",
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

/// Go: `TestShippedDockerfileIsTheOnlyOne`. deployments/docker holds exactly the image that
/// ships, and the repository no other Dockerfile (tracked or new, `Dockerfile.<name>` and
/// `<name>.Dockerfile` included): a second one would look shipped to every reader and be built by
/// nothing.
#[test]
fn shipped_dockerfile_is_the_only_one() {
    let problems = only_dockerfile_problems(&dockerfiles(&meta::repo()), SHIPPED_DOCKERFILE);
    assert!(problems.is_empty(), "{}", report(&problems));
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

/// The Rust build: on the build platform, cross-compiled by cargo-zigbuild to the target
/// platform's static musl triple, from the locked dependency graph, with every toolchain download
/// checked against its own digest in the RUN that fetches it, and stamped with the version
/// strings the binary reads.
#[test]
fn build_stage() {
    let problems = build_problems(&shipped());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The version, commit and date the binary reports come from the build: the build stage (in the
/// RUN that runs cargo zigbuild) and the make targets set the three variables src/version.rs
/// reads at compile time (service-kit's `buildinfo!`), from the same build arguments and git
/// values. A path that stamped nothing would ship a binary reporting "dev".
#[test]
fn version_is_stamped() {
    let problems = stamp_problems(&shipped());
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The posture of the image that ships: upstream's rootless Docker by its floating major tag, the
/// binary as the entrypoint and the healthcheck, a non-root default user and no TLS directory. No
/// package is installed and no volume or port is declared: the image adds the launcher to
/// upstream's and nothing else.
#[test]
fn runtime_stage() {
    let runtime = stage("runtime");
    let mut problems: Vec<String> = runtime_stage_problems(&runtime)
        .iter()
        .map(ToString::to_string)
        .collect();
    problems.extend(local_runtime_problems(&runtime));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
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
                "FROM docker:29-dind-rootless AS runtime",
                "FROM docker:29-dind-rootless@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker:29-dind-rootless AS runtime",
                "FROM $BASE AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker:29-dind-rootless AS runtime",
                "FROM docker:latest AS runtime",
            ),
            1,
        ),
        (
            alter(
                "FROM docker:29-dind-rootless AS runtime",
                "FROM docker:29-dind-rootless AS runtime\nFROM busybox:1.37 AS extra",
            ),
            1,
        ),
    ] {
        let problems = stage_problems(SHIPPED_DOCKERFILE, &altered, STAGES, BasePolicy::TagOnly);
        assert_eq!(problems.len(), want, "{}", report(&problems));
    }
}

/// The negative control of the build and stamp rules, on altered copies of the shipped
/// Dockerfile.
#[test]
fn build_rules_refuse_altered_dockerfiles() {
    let shipped = shipped();
    // The build: an unchecked download, a second fetch beside a listed one, a missing required
    // text, the build stage on the target platform, and a digest arm that is not a sha256.
    let zig_check = "\t&& echo \"${zig_sha256}  /tmp/zig.tar.xz\" | sha256sum -c - \\\n";
    for (altered, want) in [
        (alter(zig_check, ""), 2),
        (
            alter(
                "\t&& mkdir /opt/zig \\\n",
                "\t&& curl -fsSL -o /tmp/extra https://example.com/extra \\\n\t&& mkdir /opt/zig \\\n",
            ),
            2,
        ),
        (alter("--bin swarm-rootless-docker", "--bin other"), 1),
        (
            alter(
                "FROM --platform=$BUILDPLATFORM dhi.io/rust",
                "FROM dhi.io/rust",
            ),
            1,
        ),
    ] {
        let problems = build_problems(&altered);
        assert_eq!(problems.len(), want, "{}", report(&problems));
    }
    let digest = captured(
        &shipped,
        r"amd64\) zig_arch=x86_64; zig_sha256=([0-9a-f]{64}) ;;",
    );
    let unread = alter(
        &format!("zig_sha256={digest} ;;"),
        "zig_sha256=not-a-digest ;;",
    );
    assert_eq!(
        build_problems(&unread).len(),
        1,
        "{}",
        report(&build_problems(&unread))
    );

    // The stamps: one stamp out of the building RUN.
    let unstamped = alter("\t\tSWARM_ROOTLESS_DOCKER_DATE=\"${DATE}\" \\\n", "");
    assert_eq!(
        strings(&stamp_problems(&unstamped)),
        ["the RUN that runs cargo zigbuild does not stamp SWARM_ROOTLESS_DOCKER_DATE from $DATE"]
    );
}

/// The negative control of the stamp reads, on altered copies of the real src/version.rs: its
/// `buildinfo!` under another prefix, the call commented out, and a `buildinfo!` that is not
/// service-kit's each read none of the three stamps.
#[test]
fn stamp_rules_refuse_altered_version_modules() {
    let shipped = shipped();
    let version_rs = meta::repo().read(STAMPS.version_file);
    let altered = |from: &str, to: &str| {
        let altered = version_rs.replacen(from, to, 1);
        assert_ne!(
            altered, version_rs,
            "the alteration {from:?} changed nothing"
        );
        altered
    };
    let unread =
        |field: &str| format!("src/version.rs does not read SWARM_ROOTLESS_DOCKER_{field}");
    let want = [unread("VERSION"), unread("COMMIT"), unread("DATE")];
    for (name, broken) in [
        (
            "another prefix",
            altered("\x22SWARM_ROOTLESS_DOCKER\x22,", "\x22SWARM_ROOTLESS\x22,"),
        ),
        (
            "the call commented out",
            altered("pub const BUILD", "// pub const BUILD"),
        ),
        (
            "a buildinfo! that is not service-kit's",
            altered(
                "use service_kit::{BuildInfo, Defaults, EmptyStamp, buildinfo};",
                "use service_kit::{BuildInfo, Defaults, EmptyStamp};",
            ),
        ),
    ] {
        assert_eq!(
            strings(&stamp_problems_with(&shipped, &broken)),
            want,
            "{name} was accepted"
        );
    }
}

/// The negative control of the runtime and label rules, on altered copies of the shipped
/// Dockerfile.
#[test]
fn runtime_rules_refuse_altered_dockerfiles() {
    // The runtime: a forbidden instruction, a second COPY, and a pinned platform.
    for (altered, want) in [
        (alter("USER root\n", "USER root\nVOLUME /data\n"), 1),
        (alter("USER root\n", "USER root\nCOPY x /x\n"), 1),
        (
            alter(
                "FROM docker:29-dind-rootless AS runtime",
                "FROM --platform=linux/amd64 docker:29-dind-rootless AS runtime",
            ),
            1,
        ),
        (
            alter(
                "ENV DOCKER_TLS_CERTDIR=\n",
                "ENV DOCKER_TLS_CERTDIR=/certs\n",
            ),
            1,
        ),
    ] {
        let problems = runtime_stage_problems(&stage_of(&altered, "runtime"));
        assert_eq!(problems.len(), want, "{}", report(&problems));
    }

    // The image's own rules: a USER after the rootless one, an apk install, another base.
    for altered in [
        alter(
            "ENV DOCKER_TLS_CERTDIR=\n",
            "ENV DOCKER_TLS_CERTDIR=\nUSER root\n",
        ),
        alter("USER root\n", "USER root\nRUN apk add --no-cache curl\n"),
        alter(
            "FROM docker:29-dind-rootless AS runtime",
            "FROM docker:29-dind AS runtime",
        ),
    ] {
        assert_eq!(
            local_runtime_problems(&stage_of(&altered, "runtime")).len(),
            1,
            "{altered}"
        );
    }

    // The labels: one missing, one extra, one with another value.
    for (altered, want) in [
        (
            alter("\torg.opencontainers.image.licenses=\"MIT\" \\\n", ""),
            1,
        ),
        (
            alter(
                "\torg.opencontainers.image.licenses=\"MIT\" \\\n",
                "\torg.opencontainers.image.licenses=\"MIT\" \\\n\textra.label=\"x\" \\\n",
            ),
            1,
        ),
        (
            alter(
                "org.opencontainers.image.licenses=\"MIT\"",
                "org.opencontainers.image.licenses=\"Apache-2.0\"",
            ),
            1,
        ),
    ] {
        let problems = label_problems(&stage_of(&altered, "runtime"), WANT_LABELS);
        assert_eq!(problems.len(), want, "{}", report(&problems));
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

/// Exactly the expected labels (`repo_meta::image::label_problems`), and the base name naming the
/// stage's own FROM, as Docker Hub's library repository.
#[test]
fn runtime_labels() {
    let runtime = stage("runtime");
    let mut problems: Vec<String> = label_problems(&runtime, WANT_LABELS)
        .iter()
        .map(ToString::to_string)
        .collect();
    let labels = dockerfile::labels(&runtime);
    let base_name = labels.get(BASE_NAME_LABEL).map_or("", String::as_str);
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
    let content = meta::repo().read(IMAGE_FRAGMENT);
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
    let droast = meta::TOOLS.lookup(meta::DROAST)?;
    meta::TOOLS.lookup(meta::SHELLCHECK)?;
    Some(droast)
}

fn run_droast(droast: &Path, dockerfile: &Path) -> Output {
    Command::new(droast)
        .arg("--config")
        .arg(meta::repo().path(DROAST_CONFIG))
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

    let output = run_droast(&droast, &meta::repo().path(SHIPPED_DOCKERFILE));
    assert!(
        output.status.success(),
        "droast rejects {SHIPPED_DOCKERFILE}:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let shipped = meta::repo().read(SHIPPED_DOCKERFILE);
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
    let stub = path.path().join(meta::DROAST);
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
        command.env(meta::TOOLS.require_env, "1");
    } else {
        command.env_remove(meta::TOOLS.require_env);
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
        meta::TOOLS.require_env
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
