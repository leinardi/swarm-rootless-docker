# Security policy

## Supported versions

Only the latest release gets security fixes. Released `vX.Y.Z` tags are immutable and are never
re-pointed. Nothing is released yet.

## Reporting a vulnerability

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/leinardi/swarm-rootless-docker/security/advisories/new),
not in a public issue or pull request. Include the affected version, how to reproduce it, and what
an attacker gains.

This is a project maintained in spare time, so reports are handled on a best-effort basis. You will
get an answer in the advisory, and the fix, once released, is credited there unless you prefer
otherwise.

## Scope

What this project promises, and what a report against it means:

- The consumer of a daemon never gets the host's Docker daemon: no host socket, no read-write
  Docker API proxy, no privileged container.
- The daemon container always runs with the one fixed profile the README documents; no launcher
  configuration can make it privileged, add capabilities, devices or host namespaces, or mount the
  host socket or host system paths.
- The launcher removes only containers it provably owns.

A way around any of these is in scope. The daemon runs with unconfined seccomp and AppArmor and
creates user namespaces, which exposes more of the kernel to its containers than a default
container does; a kernel exploit from there lands as the daemon's unprivileged host user, and is a
kernel issue rather than a bug here. The launcher itself holds the host socket and is therefore
host-root equivalent: protect its Swarm service definition like any other host-root access.
