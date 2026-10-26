# Testing

## The layers

| Layer | Where | Runs | Covers |
| --- | --- | --- | --- |
| unit tests | `src/**/tests.rs`, `#[cfg(test)]` modules | `make rust-test` | flag rules, the daemon and probe specs (pinned whole), the probe's checks, NFSv4 ACL decoding (`testdata/nfs4/`), the launcher against a recording fake Docker |
| meta tests | `meta/tests/` | `make rust-test` | the repository itself: toolchain pins, the Dockerfile's shape, the release files, docs links, license headers, the suppression ratchet |
| image checks | `scripts/image-test.sh` | `make image-test` | the built image without a daemon: user, files, labels, entrypoint, healthcheck |
| integration tests | `tests/integration/` | `make integration-test` | the built image on a real Docker: the launcher, the probe and a rootless daemon |
| release scripts | `.github/scripts/*.test.sh` | `bash .github/scripts/<name>.test.sh` | the release plan and the scan gate, against scratch repositories and stub commands |

The launcher's unit tests drive it against `src/docker/fake.rs`, which records every call and can
hold one on a barrier: that is how a race (launcher B arriving between launcher A's create and
start) is tested deterministically, with no timing and no test hook in the shipped binary. Timers
run on tokio's paused clock.

## Integration tests

Off unless `SRD_IT=1`. They run the image `SRD_IT_IMAGE` (default: what `make image-build` builds)
against the host's Docker, one test at a time, and check among others: the exact daemon spec on
inspect, the daemon's UIDs, a consumer's `docker info`, file ownership through bind mounts, no TCP
listener, no leaked anonymous volume, stop and kill recovery, foreign and live-owner refusals,
concurrent starts on one node, the stale lock, symlinks, writable or ACL-carrying ancestors,
aliases, ranges, and the memory limit.

They need:

- the `docker` CLI on the host socket, and the image built;
- **unprivileged user namespaces**: on Ubuntu 23.10 and later, lift AppArmor's restriction for the
  run, and set it back afterwards, since it resets only at a reboot:

  ```bash
  sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
  make integration-test
  sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=1
  ```

- `SRD_IT_ROOT` (default `/srv/srd-it`): the scratch root for the host paths. Its parent must be
  root-owned and not writable by others; the tests create root-owned directories through a helper
  container on the host Docker, so they need no sudo, and they refuse a root that is not an
  absolute path of two or more plain components;
- the network, for the ACL test (it installs `acl` into an Alpine container).

`SRD_IT_REQUIRE=1` turns a test that could not run into a failure; CI sets it.

**The Swarm test** deploys a service into the node's swarm, so it is off unless `SRD_IT_SWARM=1`
as well. Turn it on only on a throwaway node: never on a machine that is part of a real swarm.

## CI

`ci.yaml`'s `ci-result` is the one required check, over:

- `pre-commit-all`: every hook on every file;
- `conventional-commits` (pull requests);
- `release-scripts`: the release scripts' tests;
- `rust`: format, clippy, the unit and meta tests (with `SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS=1`, so
  a missing tool fails a meta test instead of skipping it), cargo-deny;
- `image`: builds the 568 variant, runs the image checks, lifts the AppArmor restriction on its
  throwaway runner, initialises a single-node swarm, and runs the integration tests with
  `SRD_IT_REQUIRE=1` and `SRD_IT_SWARM=1`. A pull request from a fork has no secrets, so it builds
  the binary on the runner and hands it to the Dockerfile in place of its `dhi.io` build stage
  (`make image-build-prebuilt`).

The release runs the image checks and the integration tests again, on the exact digest it
publishes ([release.md](release.md)).

## What only the node can check

CI's runners cannot reproduce the ZFS NFSv4 ACL behaviour (the probe's decoder is tested on
fixtures captured on the node), group inheritance on the dataset, the docker socket's group on the
node, Gantry, or a multi-node swarm. When deploying, check on the node:

- the launcher becomes healthy and `docker service logs` shows dockerd's lines;
- from the consumer, `docker info` reports `rootless`, and `docker run --rm -u 0:0 -v
  "$PWD:/src" alpine touch /src/probe` leaves a file owned by the consumer;
- a non-trivial ACL on a parent of the data root makes the probe refuse the start;
- a service update replaces the daemon, and removing the service removes it.
