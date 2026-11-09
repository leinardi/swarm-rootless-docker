---
name: Bug report
about: Report a bug or unexpected behavior
title: "[Bug]: "
labels: ["bug"]
assignees: []
---
## Step 1: Are you in the right place?

* [ ] I have checked that there are no duplicate active or recent issues (bugs, questions, or
  feature requests) describing this problem.
* [ ] I am using the latest release (`ghcr.io/leinardi/swarm-rootless-docker:<major>-uid<UID>`,
  or its `vX.Y.Z-uid<UID>`), or have tested a build of `main`.
* [ ] This is not a security vulnerability (those go through the private process in SECURITY.md).

## Step 2: Describe your environment

<!-- The version is what `docker run --rm <image> --version` prints, or the image's
org.opencontainers.image.version label. Never paste a secret, such as a registry token. -->

* swarm-rootless-docker version and commit (`--version`), image tag and digest
  (`docker image inspect`): `?`
* How the launcher runs (Docker Swarm service, Docker Compose, other), its UID:GID variant and the
  service's mounts (data root, socket directory, sources): `?`
* Host Docker version (`docker version`), host kernel (`uname -r`) and the
  `kernel.apparmor_restrict_unprivileged_userns` value, if set: `?`
* Host operating system, architecture and the data root's file system (ext4, ZFS, NFS): `?`

## Step 3: Describe the problem

### Steps to reproduce

1. ---
2. ---
3. ---

### Observed results

<!-- What happened? This could be a description, error message, or log output. -->

*

### Expected results

<!-- What did you expect to happen? -->

*

### Relevant code, configuration, or data

<!-- Paste the smallest service definition or command line that reproduces the issue. Never
paste a secret, such as a registry token. -->

```yaml
# Example snippet (replace with your own)
```

### Error / log output

<!-- If you are getting an error, paste the output here. -->

```text
<paste logs or error messages here>
```

<!-- Adding screenshots, screen recordings, or additional context is always helpful. -->
