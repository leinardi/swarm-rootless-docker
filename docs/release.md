# Releases

`.github/workflows/release.yaml` builds and publishes the images. It runs **daily on a schedule**
and on a **dispatch** from `main`, with no human in the loop: the `release` environment allows
deployments from `main` only (set in the account's IaC) and has no reviewers.

## What a run releases

`.github/scripts/release-plan.sh` decides, in this order, and every lookup fails closed: an error
reading the tags, GHCR or Docker Hub stops the run instead of being read as "nothing there".

1. **recover:** the latest release is unfinished (a variant's version tag is missing on GHCR, or
   its GitHub release is missing or still a draft). The run finishes it from the tag's own commit,
   on the base its published images record, or else the one its tag records, and with the scan
   policy its tag records ([Recovery](#recovery)).
2. **head:** releasable commits since the last tag. The version is `svu next` (see the commit
   rules in [AGENTS.md](../AGENTS.md)). A schedule takes it only when CI has already passed on
   HEAD (it reads that once, and otherwise tries again the next day); a dispatch takes it, and
   the `ci-green` job then waits up to 30 minutes for CI that is still running on the commit (one
   dispatched right after a merge always finds it so), and fails when CI fails or has not finished
   by then.
3. **base** (schedule only): the latest release's own commit again, as the next patch version,
   when upstream now serves a different digest of the `docker:<major>-dind-rootless` that commit's
   Dockerfile names than the one its images were built on. This is how upstream's fixes ship:
   the image adds no package of its own.
4. **none:** nothing to do. When releasable commits wait for a green CI run, the summary says so.

A dispatch takes two inputs:

- `version`: empty derives it from the commits. An explicit one must be higher than every release;
  one that is already tagged recovers that release. The first release needs one.
- `dry_run`: plans, builds, tests, scans and rehearses the copy, and stops before the tag.

A dispatch never falls back to a base rebuild: with nothing to bump, or CI not green, it fails.

Whatever the plan picks, the `ci-green` job requires `ci-result` green on that exact commit, on
its push to `main` (gh-composite-actions' `require-ci-green`: the newest run of `ci.yaml` on that
SHA, its newest attempt, never a check name alone). For a base rebuild or a recovery the commit is
the release's own, whose push passed CI when it was released.

## What a release does

For each variant (`IMAGE_VARIANTS`, today `568:568`), in order, so that everything that can fail
does before the tag exists:

1. Check the Dockerfile's pins (`check-pinned-bases`): the runtime base and the `dhi.io` build
   stage by tag only, deliberately, the frontend by digest, every download by its sha256. The
   exemption names the runtime base's repository exactly as the commit's Dockerfile writes it:
   `docker.io/library/docker` (the canonical spelling, so nothing else on Docker Hub may float), or
   `docker` for v0.3.0 and earlier, which write `FROM docker:<major>-dind-rootless`; the plan reads
   both spellings, resolves the canonical reference, and names the build context after the
   commit's own `FROM`.
2. Build the launcher once, with CI's make target (`make image-binary`) and the commit's pinned
   toolchain, which the new tag's annotation records (a recovery switches back to the toolchain
   its tag records, and fails closed when it cannot). The musl target is added to whichever
   toolchain is active, in every mode: v0.3.0's `rust-toolchain.toml` lists no musl target, so a
   base rebuild of it would otherwise fail on a clean runner. A commit from before
   `make image-binary` builds the same binary with the cargo command its `image-build-prebuilt`
   ran.
3. Build each image from that binary, into a job-local registry, from the planned commit, with
   the base pinned: one buildx build context replaces the Dockerfile's build stage with the
   binary, so `dhi.io` is never contacted, and another replaces its
   `FROM docker.io/library/docker:<major>-dind-rootless` with the digest the plan resolved; the
   image records it in
   `org.opencontainers.image.base.digest`. The labels are checked, the layers must start with the
   base's, and the build's provenance must name exactly that base digest and the pinned frontend:
   the release notes record the base digest it read there.
4. Smoke-test that exact digest: the image's launcher is the binary built above, byte for byte,
   and `--version` prints this release's version, commit and date.
5. Run the whole image suite (the image checks, and the integration tests, Swarm included, for the
   variant they are written for) only when the plan says so: in base mode, and for a base digest
   the previous release was not built on (see below).
6. Scan it with `trivy-gate` (the latest Trivy, never pinned): a fixable HIGH or CRITICAL finding
   fails the run, the base's own included, and `.trivyignore.yaml` holds the only exceptions, each
   scoped to its paths, with a statement and an expiry. In base mode alone the gate compares the
   image with its base digest and fails only on what the image adds, reporting upstream's own as
   warnings (see below).
7. Wait for the `audit` job (cargo-deny over the commit's dependencies, run alongside the build)
   to have passed in this run attempt.
8. Push the tag (immutable), copy the tested index to GHCR by digest, untagged, and check that GHCR
   serves that digest.
9. Attest its provenance, sign it with cosign (keyless, the workflow's identity on `main`), and
   only then add `vX.Y.Z-uid<UID>`.
10. Draft the GitHub release, move `<major>-uid<UID>` when this version is the highest release of
    its Docker major, and publish the release, marked latest when it is the highest overall.

Every job that holds a write permission or an OIDC identity (`release`, `publish`, `finalize`)
runs in the `release` environment.

## What CI proves

The release does not run the test, lint and integration suites again: `ci-result` on the commit
stands for them. That holds because CI's `image` job builds what the release ships, the same way:

| Check | CI (`ci.yaml`, every variant, linux/amd64) | Release |
| --- | --- | --- |
| Dockerfile pins (`check-pinned-bases`) | yes | yes, on the commit it builds |
| fmt, clippy, unit and meta tests, docs | yes | no (`ci-green`) |
| cargo-deny | yes | yes (`audit`, the day's advisories) |
| Launcher from `make image-binary` | yes | yes |
| Image from that binary (the build context) | yes, on the base tag | yes, on the plan's base digest |
| Image's launcher is that binary (sha256) | yes (`make image-test`) | yes (smoke) |
| `--version` stamps | yes | yes (smoke) |
| Image checks (`scripts/image-test.sh`) | yes | base mode, or a new base digest |
| Integration tests, Swarm included | yes | base mode, or a new base digest |
| Trivy | no | yes, `trivy-gate` (base diff in base mode only) |

linux/amd64 is the one platform the release ships, so there is no reduced arm64 tier. The full
`make image-build` (the Dockerfile's own `dhi.io` build stage) is the local development build;
neither CI nor the release uses it.

**The base floats by tag.** Git names only `docker:<major>-dind-rootless`; each build pins the
digest that tag resolves to then. CI tested the commit on the tag as it stood when CI ran, which
may be an older rebuild of it than the one the release pins, and that tag follows upstream across
Docker's minor and patch releases, not only rebuilds. The release records the digest it shipped
(the image's label, read back from the provenance, and the release notes) and, so that no base
digest ships on a smoke test alone, runs the whole image suite itself whenever the previous
release was not built on the same digest. With the daily base rebuild keeping the latest release
on upstream's newest digest, a code release usually finds the same digest, and only smoke-tests.

**Base mode keeps the whole suite and the base diff.** A base rebuild ships the released commit
on a digest no CI run has seen, so the release itself runs the image checks and the integration
tests on it before the tag. Its scan is the image against that base digest (`trivy-gate`'s
`baseline-image`): upstream's image regularly carries fixable findings in its Alpine packages and
the Go modules of its binaries, which only upstream can fix, and a base rebuild exists to ship
upstream's fixes; gating it on an inherited finding the new digest still carries would block the
very release that brings the others. What the image adds still fails it. No other mode uses the
base diff, except the recovery of a base rebuild, which restores it from the tag (below).

## Recovery

Tags are immutable, so a failed release is never cleaned up: the next run (the next day's schedule,
a dispatch, or "Re-run all jobs" on the failed run) finishes it. "Re-run failed jobs" does not: the
release job waits for the `audit` job of its own run attempt, and an earlier attempt's pass never
counts, so it would wait and fail. A recovery reuses a published variant only when it carries this
workflow's provenance for `main`, was built from the tagged commit and on the recorded base, and is
signed by this workflow; it rebuilds a variant that is missing, and stops for a human on anything
else (an image this workflow did not produce holding a version tag). An untagged digest a failed run
left on GHCR is inert.

**The tag records how its release was built and scanned.** Next to the toolchain, a new tag's
annotation carries two lines, written and read by the same script (`release-plan.sh record`, and
its `recover`):

```text
Base: docker.io/library/docker:29-dind-rootless@sha256:<digest>
Scan baseline: docker.io/library/docker@sha256:<digest>
```

`Scan baseline` is `none` outside base mode. A recovery builds on the recorded base digest when no
variant is published yet (one that is must agree with it, or the run stops), and scans with the
recorded baseline. That matters when a base rebuild fails right after its tag push: the next run is
a recovery, and with a stricter gate than the base diff it passed it would refuse the upstream
findings that rebuild accepted, never finish, and hold back every later scheduled release. A tag
without the lines (v0.3.0 and earlier) is a release on an unrecorded base, scanned with no
baseline, as it was. A line that does not parse, or a baseline that is not the recorded base,
stops the run.

A wrong release is answered with a higher version, never a moved tag.

## Docker majors

Git names the base only by its floating major tag, `docker.io/library/docker:29-dind-rootless`, in
the Dockerfile;
no other file names a major (a meta test checks it, and that the README and the Compose example
name the Dockerfile's). Dependabot proposes the next major (`30-dind-rootless`) as a pull request,
merged by hand, as a `feat(deps)`: its release starts the new floating tag `30-uid568`, and
consumers move to it deliberately. Base rebuilds always follow the major of the commit they
rebuild.

## First release

1. Dispatch with `version` `0.1.0` and `dry_run` on.
2. Dispatch again with `dry_run` off.
3. Make the GHCR package public (or give the nodes a pull credential): a new package may start
   private.
4. Check it: `gh attestation verify` and `cosign verify` (the [README](../README.md#images) has
   both), and that `29-uid568` resolves to the release's digest.

## Scheduled scan

`.github/workflows/scheduled-scan.yaml` runs weekly: `trivy-gate` over the image behind each
floating tag, every finding counted, the base's included, as the release gates outside base mode,
and cargo-deny over `main`'s dependencies. A failure fails the run: fix it with a release, or, for
a finding only upstream's base can fix, with a reviewed `.trivyignore.yaml` entry that expires,
until the daily base rebuild ships upstream's fix.
