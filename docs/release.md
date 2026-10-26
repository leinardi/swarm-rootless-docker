# Releases

`.github/workflows/release.yaml` builds and publishes the images. It runs **daily on a schedule**
and on a **dispatch** from `main`, with no human in the loop: the `release` environment allows
deployments from `main` only (set in the account's IaC) and has no reviewers.

## What a run releases

`.github/scripts/release-plan.sh` decides, in this order, and every lookup fails closed: an error
reading the tags, the checks, GHCR or Docker Hub stops the run instead of being read as "nothing
there".

1. **recover:** the latest release is unfinished (a variant's version tag is missing on GHCR, or
   its GitHub release is missing or still a draft). The run finishes it from the tag's own commit,
   on the base its published images record.
2. **head:** releasable commits since the last tag, and `ci-result` green on that exact HEAD
   commit. The version is `svu next` (see the commit rules in [AGENTS.md](../AGENTS.md)). A
   dispatch waits up to 30 minutes for CI that is still running on the commit (one dispatched
   right after a merge always finds it so) and fails only when CI fails, or has not finished by
   then; a schedule does not wait, and tries again the next day.
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

## What a release does

For each variant (`IMAGE_VARIANTS`, today `568:568`), in order, so that everything that can fail
does before the tag exists:

1. Build once, into a job-local registry, from the planned commit, with the base pinned: a buildx
   build context replaces the Dockerfile's `FROM docker:<major>-dind-rootless` with the digest the
   plan resolved, and the image records it in `org.opencontainers.image.base.digest`. The image's
   labels are checked, and its layers must start with the base's.
2. Test that exact digest: the image checks, and the integration tests (Swarm included) for the
   variant they are written for.
3. Scan it. The gate (`.github/scripts/scan-findings.sh`) fails on a fixable HIGH or CRITICAL
   finding the image adds to its base, and reports the base's own as warnings: upstream's image
   regularly carries fixable findings in its Alpine packages and the Go modules of its binaries,
   which only upstream can fix, and gating on them would block the very rebuilds that ship their
   fixes. `.trivyignore.yaml` holds the only other exceptions, each with a reason and an expiry.
4. Push the tag (immutable), copy the tested index to GHCR by digest, untagged, and check that GHCR
   serves that digest.
5. Attest its provenance, sign it with cosign (keyless, the workflow's identity on `main`), and
   only then add `vX.Y.Z-uid<UID>`.
6. Draft the GitHub release, move `<major>-uid<UID>` when this version is the highest release of
   its Docker major, and publish the release, marked latest when it is the highest overall.

## Recovery

Tags are immutable, so a failed release is never cleaned up: the next run (the next day's
schedule, or a dispatch) finishes it. A recovery reuses a published variant only when it carries
this workflow's provenance for `main`, was built from the tagged commit and on the recorded base,
and is signed by this workflow; it rebuilds a variant that is missing, and stops for a human on
anything else (an image this workflow did not produce holding a version tag). An untagged digest a
failed run left on GHCR is inert.

A wrong release is answered with a higher version, never a moved tag.

## Docker majors

Git names the base only by its floating major tag, `docker:29-dind-rootless`, in the Dockerfile;
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

`.github/workflows/scheduled-scan.yaml` runs weekly: the same scan gate over the image behind each
floating tag, against the base it records, and cargo-deny over `main`'s dependencies. A failure
fails the run; fix it with a release.
