# AGENTS.md

## What this is

A container image plus a small Rust launcher that gives a Docker Swarm service its **own dedicated
rootless Docker daemon** on the same node, without ever exposing the host's Docker daemon to that
consumer. The launcher runs as an unprivileged Swarm service with only the host socket, and creates
and supervises the daemon container from its own image digest, so Swarm owns the lifecycle and image
updates and rollbacks reach the daemon. The daemon container always gets one fixed security profile
(no `--privileged`, no added capabilities beyond `SETUID`/`SETGID`, no host namespaces).

**State:** the launcher, the image, the integration tests and the release workflow are in place;
the first release is next. [`README.md`](README.md) is the user-facing contract: keep it in step
with what the code does. [`docs/architecture.md`](docs/architecture.md) explains the launcher's
lifecycle and ownership rules, [`docs/release.md`](docs/release.md) the releases and their
recovery, [`docs/testing.md`](docs/testing.md) the test layers.

## Common commands

```bash
make rust-build    # cargo build --workspace --all-targets --all-features --locked, unstamped
make rust-test     # cargo test --workspace --locked (the meta-tests in meta/ included)
make rust-fmt      # cargo fmt --all; rust-fmt-check only checks
make rust-clippy   # cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
make verify        # verify-rust: rust-build, rust-fmt-check, rust-clippy, rust-test and rust-doc
make rust-doc      # cargo doc --workspace --all-features --no-deps --locked, warnings as errors
make rust-run ARGS="--version"   # run with SWARM_ROOTLESS_DOCKER_VERSION/_COMMIT/_DATE set from git
make rust-deny     # cargo deny --workspace check: advisories, licences, bans, sources (network)
make audit-deps    # cargo deny check advisories (network required)
make check         # pre-commit on all files: rustfmt, clippy, tests, droast, dclint, linters
make check-stage   # pre-commit on the staging area only
make help          # lists every target
```

Before calling a change done, run the rows of the [validation matrix](#validation-matrix) that
match it.

Single test (an integration test is named by its file):

```bash
cargo test --locked -p meta --test toolchain
```

The Makefile pulls shared snippets from `leinardi/make-common@v1` into `.mk/` on first run. To
refresh: `make mk-common-update`. The Rust targets are make-common's `rust.mk`, set up in the
Makefile's project config (`RUST_STAMP_PREFIX`). Project targets live in the
local `.mk/*.mk` files listed in `MK_LOCAL_FILES` (`verify.mk`, `audit-deps.mk`, `image.mk`),
never as recipes in the Makefile.

The image and its integration tests:

```bash
make image-build            # the 568 variant (ROOTLESS_UID/ROOTLESS_GID); needs a dhi.io login
make image-binary           # the static launcher the release ships, into target/image-context/out
make image-build-prebuilt   # the image from that binary: the release's packaging path, no dhi.io
make image-test             # the built image without a daemon (IMAGE_TEST_BINARY: compare its binary)
make image-build-test       # image-build, then image-test (ordered, safe under make -j)
make integration-test       # tests/integration.rs on this Docker; needs the AppArmor sysctl at 0
```

In CI, `ci-result` is the one required check, and the one the release gates on: it needs
`pre-commit-all`, `conventional-commits`, `release-scripts`, `rust` and `image` (`ci.yaml`); the
reviewdog jobs only annotate pull requests. `image` tests the release's packaging path for every
variant: `make image-build-prebuilt`, `make image-test` with `IMAGE_TEST_BINARY` (the image's
launcher must be that binary), and the integration tests; it needs no secret. `release-scripts`
runs the release plan's tests:

```bash
bash .github/scripts/release-plan.test.sh    # the release plan, against scratch repositories
```

Releases are cut by `release.yaml`: daily on a schedule, or dispatched on `main` (with an
explicit version for the first one, and a dry run first). `release-plan.sh` decides what a run
releases: HEAD as a new version, the latest release's own commit as a patch when upstream's base
has a new digest, the finishing of a release that failed part-way, or nothing. Whatever it picks,
`ci-result` must be green on that exact commit (gh-composite-actions' `require-ci-green`), and
the release runs a smoke test and the Trivy gate rather than the suites CI ran, except in base
mode and for a base digest no release was built on, where it runs the whole image suite
([`docs/release.md`](docs/release.md)).

## Layout

A Cargo workspace: the root package `swarm-rootless-docker` (the binary) and `meta`.
`rust-toolchain.toml` pins the toolchain (release, components and target; CI installs it with
`rustup toolchain install`), `rustfmt.toml` and `clippy.toml` sit at the root,
`deny.toml` configures cargo-deny, and the lint set is `[workspace.lints]` in the root
`Cargo.toml`.

- `src/` — the one binary. `main.rs` runs the mode `cli.rs` parses: `launcher/` (the `launch` mode
  and the probe run), `daemon/` (the `daemon` and `daemon-health` modes), `probe/` (the host-side
  checks, NFSv4 ACL decoding included) and `status/` (the status socket the `healthcheck` mode
  asks). Around them: `config/` (every flag as a validated type), `spec/` (each container the
  launcher creates, from one function, and the labels), `node/` (the node lock and stale rules),
  `docker/` (the Docker API behind a trait: bollard through rust-common's `docker-client`, and the
  recording fake for tests), `mounts/`
  (where a host path lies, from `mountinfo`), `socket/`, `output.rs` (stdout and stderr off the
  runtime), `logger.rs` and `version.rs`.
- `tests/integration/` — the integration tests against the built image (`docs/testing.md`);
  `testdata/nfs4/` — NFSv4 ACL fixtures captured on the target node.
- `deployments/docker/` — the `Dockerfile` (the shipped image) and `docker-compose.yaml` (the
  Swarm stack example the README walks through).
- `meta/` — the repository meta-tests, `publish = false`: `meta/tests/toolchain.rs` (the Rust line
  and the exact Rust release, ShellCheck one value, Trivy never pinned, no vacuous flags),
  `dockerfiles.rs` (the image's stages and runtime rules), `release.rs` (no Docker major outside the
  Dockerfile, the image workflows agree on the variants, every variant scanned), and
  `workspace_lints.rs` (every member opts into the workspace lints). Most run rust-common's
  `repo-meta` checks with this repository's parameters, beside the rules that are its own.
  `meta/src/lib.rs` holds what they share: `repo()`, the repository under test.
- `.github/workflows/` — `ci.yaml` (the gates and `ci-result`), `release.yaml` (plan, build,
  test and scan each variant, tag, publish to GHCR with provenance and a cosign signature, move the
  floating tags), `scheduled-scan.yaml` (weekly: the images behind the floating tags, and
  cargo-deny) and `pre-commit-warmup.yaml` (refreshes the pre-commit cache on `main`).
  `.github/scripts/` holds the release's own script and its test: `release-plan.sh` (what to
  release). The shared steps come from `leinardi/gh-composite-actions`, pinned by SHA:
  `require-ci-green`, `require-job-success`, `check-pinned-bases`, `trivy-gate` (the Trivy gate,
  with the base diff in base mode), `toolchain-record`/`toolchain-recover`, `setup-svu`,
  `setup-cargo-deny` and `setup-shellcheck` (the pinned ShellCheck droast's required bridge runs).
  `.trivyignore.yaml` lists the scans' accepted findings.
- `droast.toml` — the Dockerfile Roast policy (the seven required OCI labels, strict labels, a
  required ShellCheck bridge); `.dclintrc` — the Compose lint rules.
- `scripts/` — `image-test.sh` (`make image-test`), `check-license-headers.sh`,
  `hooks/rustfmt-on-edit.sh` (the Claude Code edit hook in `.claude/settings.json`) and
  make-common's `bootstrap-mk-common.sh`.

### Generated or vendored: never hand-edit

- `.mk/{help,password,pre-commit,rust}.mk`, `.mk/.mk-common-version` and
  `scripts/bootstrap-mk-common.sh`: fetched from make-common; refresh them with
  `make mk-common-update`, and port a change upstream.
- `Cargo.lock`: written by cargo; change it with `cargo update -p <crate>`, never by hand.

### Shared crates (rust-common)

`repo-meta` and `testenv-core` (the meta crate's dependencies, default features), `tracing-setup`
(the logger's subscriber, written to its `Lane`; the root `[dev-dependencies]` add its `testing`
feature for the logger tests' `Capture`), `service-kit` (`buildinfo!` only, no features) and
`docker-client` (feature `attach`) come from
[leinardi/rust-common](https://github.com/leinardi/rust-common), a git dependency pinned by
release tag (`tag = "v1.0.0"`, with `version = "1.0"`) in the root `Cargo.toml`'s
`[workspace.dependencies]`.
Each member opts in with `<name>.workspace = true`.

- **A behaviour difference is a parameter of the shared crate**, passed from here, never a fork of
  its code into this repository. A fix to shared code lands in rust-common first, then reaches here
  by moving the `tag`.
- **Moving the `tag`** needs the network: change it on every rust-common line (nothing checks that
  they agree), and `version` with it when the crates' `<major>.<minor>` moves (a `v1.0.0` checkout
  does not resolve against `version = "0.3"`), then run `cargo fetch` before
  the `--locked` make targets, which otherwise refuse the stale lock. The manifest change is what
  makes cargo re-resolve the git source; `cargo fetch` changes nothing else, whereas
  `cargo update -p <crate>` can also move unrelated edges.
  `git diff Cargo.lock` must touch only rust-common lines. Commit the `Cargo.toml` and
  `Cargo.lock` changes together.
  `deny.toml` allow-lists the source (`allow-git`); it changes with the URL. `make rust-deny`
  checks every workspace member's graph (`--workspace`), the meta crate's rust-common crates
  included.
- **Local work on a shared crate** uses an uncommitted
  `[patch."https://github.com/leinardi/rust-common"]` table in the root `Cargo.toml`, one
  `<name> = { path = "../rust-common/crates/<name>" }` line per crate the workspace names (a
  patched crate's own rust-common dependencies are path dependencies there, so they follow). The
  patch re-locks the crates, so refresh `Cargo.lock` once without `--locked` (`cargo fetch`). When
  done, remove the table and `git checkout Cargo.lock`: neither the table nor the patched lock is
  ever committed, since CI and the image build cannot see the path.

This repository's intentional differences from the other Rust services:

- **`docker-client`, not rust-common's `docker-engine`:** the launcher was never a Go port, so it
  has no Go client's wire behaviour to keep. It uses `docker-client` (feature `attach`) for the
  connection, the whole-call bound, the error kinds and the attach and wait pumps, and bollard
  comes through it (`docker_client::bollard`); the `DockerApi` trait, its own types and
  `DockerError` stay here.
- **No wall-clock ban** (no go-clock, no `disallowed_methods` for `SystemTime`): the launcher's own
  code reads no calendar time (only the log timestamps do, through tracing-subscriber); its
  deadlines are monotonic, and the launcher and node tests drive the timers with the paused tokio
  clock.

## Validation matrix

Run before considering an edit done. `make check` applies to every row.

| You edited | Run |
| --- | --- |
| `**/*.rs` | `make verify`, then `make check-stage` |
| `Cargo.toml`, `meta/Cargo.toml`, `Cargo.lock`, `deny.toml` | `make verify`, `make rust-deny` (network; the commit hook runs it too), `cargo test --locked -p meta --test toolchain --test workspace_lints` |
| the rust-common `tag` in the root `Cargo.toml` | `cargo fetch` (only rust-common lines change in `Cargo.lock`), then `make verify` and `make rust-deny`, with the network ([Shared crates](#shared-crates-rust-common)); the `[patch]` table and a patched `Cargo.lock` are never committed |
| `rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml` | `make verify`, `cargo test --locked -p meta --test toolchain` |
| `scripts/check-license-headers.sh` | `scripts/check-license-headers.sh` |
| `scripts/hooks/rustfmt-on-edit.sh` | shellcheck (in `make check`); run it once by editing a `.rs` file in Claude Code |
| `droast.toml`, `.dclintrc` | `make check` (droast and dclint) |
| `deployments/docker/Dockerfile`, `scripts/image-test.sh` | `make image-build-test`, `cargo test --locked -p meta --test dockerfiles --test release`; a new Docker major also updates the README and the Compose example (the release meta test names them) |
| `tests/integration/**`, the launcher's behaviour | `make integration-test` (the AppArmor sysctl at 0 for the run, then back to 1; `docs/testing.md`) |
| `deployments/docker/docker-compose.yaml` | `make check` (dclint); `docker stack config -c deployments/docker/docker-compose.yaml` with `DOCKER_GID` set; keep the README's deployment section in step |
| `.github/workflows/**`, `.github/actions/**`, `.github/dependabot.yml` | `pre-commit run actionlint --all-files`, `cargo test --locked -p meta --test toolchain --test release`; reason through each trigger the workflow has (pull request, push, dispatch, schedule) and keep `ci-result`'s `needs` complete; for `release.yaml`, dispatch a dry run |
| `.github/scripts/**` | `bash .github/scripts/<script>.test.sh` for each script changed, `cargo test --locked -p meta --test release` |
| `.trivyignore.yaml` | every entry scoped to paths, with a statement and an `expired_at`; scan the image with the latest Trivy and this file, as trivy-gate does (`trivy image --ignore-unfixed --severity HIGH,CRITICAL --ignorefile .trivyignore.yaml`) |
| `.pre-commit-config.yaml`, `Makefile`, `.mk/**` | `make check`, `cargo test --locked -p meta --test toolchain`, `make help`; a hook that needs a system tool also gets it in `ci.yaml`'s `pre-commit-all` |
| `scripts/bootstrap-mk-common.sh`, the make-common `.mk` files | never by hand: port the change to make-common, then `make mk-common-update` and `make help` |
| `meta/**` | `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1 cargo test --locked -p meta` |
| `**/*.md`, `.agents/skills/**` | `pre-commit run lychee --all-files` (offline link and fragment check); prose wraps at 100 columns (markdownlint) |

## Rust coding rules

Use the maintainer's user-level Rust skills for general coding guidance. The lints live once,
in `[workspace.lints]` of the root `Cargo.toml`, with their settings in `clippy.toml`; every member
declares `[lints] workspace = true`, and every lint and test command runs workspace-wide. In short:

- **Lint baseline:** `unsafe_code` is forbidden; `clippy::pedantic` warns, and clippy runs with
  `-D warnings`.
- **Suppressions:** only `#[expect(lint, reason = "...")]`. `#[allow]` is denied
  (`allow_attributes`), and so is a suppression without a reason. The reason says why the fix does
  not apply here, not which lint fired.
- **No panics outside tests:** `unwrap_used` and `expect_used` are denied (tests may use both).
  Return an error, one `thiserror` enum per module.
- **No direct printing:** `print_stdout` and `print_stderr` are denied. Log through `tracing`; the
  version output is an explicit `writeln!` to stdout.
- **Exhaustive matches:** `wildcard_enum_match_arm` is denied: a match on an enum names every
  variant.
- **Descriptive names:** `min_ident_chars` refuses single-letter names. Prefer `index`, `state`,
  `container_id`.
- **Item order:** public items before private ones within an `impl`.
- **Tests:** unit tests sit in a `#[cfg(test)] mod tests` beside the code (or a `tests.rs` in the
  module directory). Timers are driven by the paused tokio clock, not by sleeping. A test ported
  from swarm-device-access (Go) carries a doc comment naming the Go test it replaces
  (``/// Go: `TestRun_CallOrder` ``).

## Conventions worth knowing

- **Fail closed.** An empty, unknown or malformed flag, label or probe result refuses; it never
  falls back to a wider daemon profile, and the launcher never touches a container it does not
  provably own.
- **Version strings:** `src/version.rs` reads `SWARM_ROOTLESS_DOCKER_VERSION`,
  `SWARM_ROOTLESS_DOCKER_COMMIT` and `SWARM_ROOTLESS_DOCKER_DATE` at compile time (service-kit's
  `buildinfo!`), falling back to `dev`, `none` and `unknown`; an empty variable stamps the empty
  string, and `long()` formats the version line here. `make rust-run` sets them from git, the date
  from HEAD's commit (make-common's `.mk/rust.mk`, with `RUST_STAMP_PREFIX := SWARM_ROOTLESS_DOCKER`
  and `BUILD_DATE` in the Makefile), and so do `make rust-dist`, `make image-binary` and
  `make image-build-prebuilt`; `make rust-build` builds unstamped, as the tests do
  (`RUST_BUILD_ENV :=`).
- Every Rust and shell file, the Makefile and the repo-local `.mk` fragments carry the MIT license
  header, with the year of the file's first commit (copy it from any Rust or shell file, or use the
  IntelliJ MIT copyright profile in `.idea/copyright/`). The `check-license-headers` pre-commit hook
  (`scripts/check-license-headers.sh`) checks the whole tree; the make-common files are exempt.
- `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1` (set by the `rust` CI job) turns a missing external tool
  a unit test drives (`setfacl`, in the probe's tests) into a failure instead of a skip.
- Pin every action and reusable workflow by full commit SHA, with the version in a trailing comment
  (`@<sha> # vX.Y.Z`). Give every workflow a top-level `permissions: contents: read`, and widen it
  only on the job that needs more. Pass `${{ }}` expressions into `run:` scripts through `env:`.
- `.serena/` is Serena's own config and memories; pre-commit excludes it.

## Markdown

Prose wraps at 100 columns (markdownlint `line-length`, `.editorconfig`); tables, code blocks and
headings may run longer. Never wrap inside a table, a code block or a link target.

## Project skills

Skills live in `.agents/skills/` (symlinked as `.claude/skills`). Load them before the work, not
after review:

- `adversarial-review` — for any review request ("review my diff", "is this ready to merge").

## Commit messages

All commits MUST be Conventional Commits 1.0.0 **with a scope**:
`<type>(<scope>)[!]: <description>`, optional blank-line body and footers. Enforced by the
`conventional-pre-commit` `commit-msg` hook (`--force-scope`, installed by
`make pre-commit-install`) and by the `conventional-commits` CI job. Types: `feat`, `fix`, `docs`,
`test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`, `revert`. Breaking changes use `!`
before `:` or a `BREAKING CHANGE:` footer. Release notes are not built from these messages:
`gh release create --generate-notes` lists the merged pull requests by title. Examples:
`fix(launcher): stop the daemon by ID`, `feat(image): add the uid 1000 variant`.

Release versions are derived by `svu` from the commits since the last `vX.Y.Z` tag, so a wrong type
ships a wrong version:

| Release | Commit | Example |
| --- | --- | --- |
| major | any type with `!` before the colon, or a `BREAKING CHANGE:` footer | `feat(launcher)!: rename the --data-root flag` |
| minor | `feat` | `feat(image): add the uid 1000 variant` |
| patch | `fix` | `fix(launcher): stop the daemon by ID` |
| none | everything else: `perf`, `refactor`, `build`, `ci`, `chore`, `docs`, `style`, `test`, `revert` | `docs(readme): fix a link` |

The highest bump among the commits wins; with only "none" commits since the last tag, a release
with no version fails with "nothing to bump".

**Pick the type by whether the change should ship, not by what kind of change it is.** Anything
that changes the shipped image and that users should receive is `fix` (or `feat`), even when it is
a performance improvement, a refactor or a revert. Use `perf`, `refactor`, `style` and `revert`
only when the commit is deliberately not meant to trigger a release on its own. A `revert` of a
shipped `feat` or `fix` is itself a `fix`. `svu` matches `feat`/`fix` anywhere in the subject (e.g.
`prefix:` counts as `fix:`), so avoid a word ending in `feat` or `fix` directly before a colon in
other subjects. PRs land as merge commits, so every commit counts, not just the PR title.
