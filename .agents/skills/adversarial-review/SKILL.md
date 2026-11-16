---
name: adversarial-review
description: >
  Adversarial code review of changes to swarm-rootless-docker — working tree, staged diff, a
  branch vs main, a commit range, or a PR — in any language (Rust, bash, Dockerfile, Makefile,
  YAML, docs). Uses user-level Rust skills for Rust paths, hunts for real defects and for anything
  that widens the daemon container's fixed security profile, lets the launcher touch a container
  it does not own, races the node lock, trusts a lexical path check as a boundary, or ships the
  wrong image or release, then reports ranked findings with a verdict. Use when the user asks to
  review changes, a diff, a PR or a branch, to "check my work before committing", whether it "is
  ready to merge", or to "poke holes in this" — even if they don't name a language or say the
  word "review".
---

# Adversarial Review — swarm-rootless-docker

You are a hostile reviewer. Assume the change is **wrong until proven right** and find the
input, state or event where it breaks. A review that finds nothing is only credible after you
tried to break it and failed. Read `AGENTS.md` first: its conventions are the review's floor.
Then read `README.md` and `SECURITY.md`: they are the user-facing contract a change must keep
true. For any `**/*.rs` path (and `Cargo.toml`, `clippy.toml`, `rustfmt.toml`, `deny.toml`), load
the user-level Rust skills before judging it; project rules are findings even when clippy is
green. Every other path gets the adversarial passes and the invariants below with the same rigor.

## Scope

When invoked directly: review the scope the user gives (`gh pr diff <n>` for a PR); with none,
the uncommitted work (`git diff HEAD` plus untracked files); on a clean tree, `git diff
main...HEAD`. Read every changed file in full, the files that call or document it, and the
stated intent (`git log --oneline` for the range): the change is checked against it.

## Repository invariants

### The daemon container's profile is fixed

The security boundary of the whole project. Any widening is **critical**.

- The daemon container gets exactly: `Privileged=false`, `CapDrop=[ALL]`,
  `CapAdd=[SETUID, SETGID]`, `SecurityOpt=[seccomp=unconfined, apparmor=unconfined,
  systempaths=unconfined]`, one device `/dev/net/tun`, the image's labelled non-zero UID:GID
  with no `GroupAdd`, private PID/IPC/cgroup namespaces, default userns, no port bindings, no
  host network. `no-new-privileges` is absent on purpose (it disables `newuidmap`'s file caps).
- No configuration path (flag, label, environment, compose file) can add a capability, a
  device, a host namespace, `privileged`, a host-socket mount, or a host system path; a new flag
  is a widening and needs a reason in the README and `SECURITY.md`.
- The spec is built from validated types in one function and pinned whole by a unit test; a
  change to that test without a matching docs change is a finding.
- `Mounts`, never `Binds`. The image's two `VOLUME`s are covered. No TCP listener: the daemon
  command is fixed and never reaches the upstream entrypoint's default-arguments path.
- `/etc/subuid` and `/etc/subgid` in the daemon container are root-owned and unwritable by the
  daemon UID.

### The launcher touches only what it provably owns

- Stops and removes go **by ID**. A container is removed only when its owner launcher is
  confirmed NotFound or not running; an inspect error, a nil state or a foreign container
  refuses.
- The node lock (a never-started lock container) is held across list, probe, validate and
  create, released on every path, and recovered only when its holder is confirmed gone.
- Every listing that decides a reservation uses `all = true`: a created, not started daemon is a
  reservation.

### Lexical checks are not the boundary

- A path decision that relies on the launcher's own view of a host path, or on a lexical
  denylist, is a finding: Docker resolves bind sources on the host. The host-side probe decides
  (no symlinks, root-owned trivial-ACL ancestry, inode identities, host ID ranges), and any
  probe failure refuses.
- The subordinate range never overlaps a host user, a host subordinate entry, another
  reservation, 0 or the consumer's UID/GID.

### Shutdown and streams

- One shutdown budget from the signal; no per-step fresh grace period; the budget stays under
  the documented `stop_grace_period`.
- Every Docker call has a timeout; stream setups are raced against timeout and cancellation;
  the exit status and the last log lines survive SIGTERM.

### Images and releases

- The base is the floating `docker.io/library/docker:<major>-dind-rootless` (v0.3.0 and earlier
  write `docker:<major>-dind-rootless`; both must still be read) from the Dockerfile of the commit
  being built, pinned per build to the digest the release resolved, and recorded in the image. Every
  release needs a green `ci-result` on the exact SHA it builds (`require-ci-green` in the ci-green
  job, never a check name); a base-only rebuild builds the last release's SHA with that SHA's own
  base, context key and floating tag, never HEAD's, and runs the whole image suite and the base-diff
  scan itself. A recovery restores the base digest and the scan policy its tag's annotation records
  (`Base:`, `Scan baseline:`); a tag without them means no baseline. The image's launcher is the
  binary `make image-binary` built, byte for byte, in CI and in the release.
- Tags are immutable; a floating `<major>-uid<UID>` tag only moves to the highest release of
  that Docker major. Nothing is tagged before the image is tested and scanned.

### Workflows

- Every action and reusable workflow is pinned by full commit SHA, with the version in a
  trailing comment that matches the SHA. A tag or branch ref is a finding.
- Top-level `permissions: contents: read`; a job that needs more says so on the job, and no
  more than it uses. A write permission on a job that runs pull-request code is **critical**.
- No `${{ }}` expression inlined in a `run:` script or a `github-script` body: it goes through
  `env:`. Inlined attacker-controlled values (titles, branch names, commit messages, inputs) are
  script injection, **critical**.
- A job gated on `github.event_name == 'pull_request'` must not be a required check that other
  events need to report.
- `ci-result` is the one required check. A gating job missing from its `needs` gates nothing; a
  result other than `success` may pass it only for `conventional-commits`, skipped off pull
  requests. `pre-commit-all` installs every tool its hooks need, and its `SKIP` lists only hooks a
  dedicated job runs.
- A release or other writing workflow refuses to run from anything but the default branch.
- Reason through every trigger a changed workflow has: pull request, push, dispatch, schedule.

### Shell and make

- Quoting: paths with spaces, empty variables, `$$` escaping in make recipes, a `\` continuation
  on every line of a multi-line recipe.
- Failure: `set -euo pipefail` in scripts; a step that exits 0 after a failed command (`;`
  instead of `&&`, a pipeline without `pipefail`, `|| true` that hides a real error).
- New targets live in a local `.mk/*.mk` listed in `MK_LOCAL_FILES`, never in the Makefile; a
  fetched `.mk` file edited by hand is lost on the next `make mk-common-update`.

### Commits and releases

Every commit is `type(scope): subject`, and the type decides the release. `svu` derives it from
the commits since the last tag, and every commit lands as it is (merge commits). A change users
should receive typed `chore`, `ci`, `docs`, `refactor` or `perf` never ships; a `feat` or `fix`
on CI, tests or docs cuts a release for nothing; a stray `!` or `BREAKING CHANGE:` cuts a major.
A `revert` of a shipped `feat` or `fix` is itself a `fix`. `svu` also matches `feat`/`fix`
anywhere before a colon (`prefix:` counts as `fix:`). See `AGENTS.md`.

## Adversarial passes

- **Defaults:** a default that changes what an existing user gets.
- **Empty and edge input:** unset optional inputs, a repository with no tags, an empty list, a
  first run, a re-run after a partial failure.
- **Contract drift:** README, `AGENTS.md`, `CONTRIBUTING.md` and the PR template still describe
  what the repository does.
- **A check that can no longer fail:** a hook, script or CI job guards a claim nobody re-checks by
  hand. Make the guarded thing go wrong and see whether the check still notices. A hook whose
  `files:` pattern matches nothing, a whole-tree hook that lost `always_run` or `stages`, a check
  that passes on an empty file list, a `pre-commit-all` that skips the hook, or a CI step with
  `|| true` or `continue-on-error`, is a false pass: a finding.

## Gates

Do not re-run the repository's gates (`make check`, `make verify`, `make rust-deny`, `cargo test -p
meta`, the integration tests). The implementer is expected to run them and CI runs them again, but
do not assume either happened: report the verification evidence you actually have (under
adversarial-review-loop, `verification.txt` when supplied, which is the implementer's claim and not
proof; when invoked directly, the CI status or gate output in the PR or session) and what is
missing, such as a launcher, spec, probe or image change with no integration run (`SRD_IT=1`, a host
with Docker) reported, or a `meta/**` change whose run did not set
`SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1` (without it the meta-tests may skip their tools). Judge what
is missing against `AGENTS.md`'s *Validation matrix*: every changed path owes the checks its row
names. Never say that a check you did not see passed. Judge whether the change's tests prove it: a
behaviour change needs a test that fails on the old code. When invoked directly, one focused test
(`cargo test --locked --lib <module>::tests::<name>`) may be run to confirm a single suspected
finding, never a full suite or gate. A candidate that is neither reproduced nor traced end to end is
dropped.

## Report

Rank worst first:

```text
<path>:<line> — <severity: critical | high | medium | low>: <one-line defect>
  Failure: <the concrete input/state → the wrong result or broken invariant>
  Fix: <the specific change>
```

End with a verdict (**block**, **approve with nits** or **approve**) and one line:
`Verification evidence: seen … / missing …`.
