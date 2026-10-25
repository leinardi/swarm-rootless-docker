# swarm-rootless-docker

A container image plus a small Rust launcher that gives a Docker Swarm service its **own dedicated
rootless Docker daemon** on the same node, without ever exposing the host's Docker daemon to that
consumer.

> **Status: under construction.** This repository holds the scaffolding so far; the launcher, the
> image and its documentation are being written. Nothing is published yet.

## Why

A service that needs Docker (builds, `docker run`, Compose, image tests) must not get the host's
Docker socket, a read-write Docker API proxy or a `--privileged` container: each of those is root on
the host. Upstream `docker:dind-rootless` runs unprivileged with a small set of options, but Docker
Swarm ignores `security_opt`, so it cannot be a Swarm service itself.

## How it will work

- **Image:** `docker:<major>-dind-rootless` with its `rootless` user renumbered to the consumer's
  UID, so files the daemon's containers write into the consumer's bind mounts come back owned by
  the consumer, plus the launcher binary. Published to GHCR per Docker major and UID.
- **Launcher:** an unprivileged Swarm service with only the host socket (non-root, all capabilities
  dropped, no network). It creates the daemon container from its own image digest on the same node,
  supervises it, streams its logs and stops it on shutdown, so Swarm owns the lifecycle and image
  updates and rollbacks reach the daemon.
- **Fixed profile:** the daemon container always gets the same verified set (unconfined seccomp,
  AppArmor and system paths, `/dev/net/tun`, the consumer UID, only `SETUID` and `SETGID`) and never
  `--privileged`, extra capabilities or devices, host namespaces, or a mount of the host socket or
  host system paths. No TCP listener: a Unix socket the consumer mounts read-only.
- **One daemon per consumer**, never shared.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md). Security reports:
[SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).
