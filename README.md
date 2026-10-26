# swarm-rootless-docker

A container image plus a small Rust launcher that gives a Docker Swarm service its **own dedicated
rootless Docker daemon** on the same node, without ever exposing the host's Docker daemon to that
consumer.

## Why

A service that needs Docker (builds, `docker run`, Compose, image tests) must not get the host's
Docker socket, a read-write Docker API proxy or a `--privileged` container: each of those is root on
the host. Upstream `docker:dind-rootless` runs unprivileged with a small set of options, but Docker
Swarm ignores `security_opt`, so it cannot be a Swarm service itself.

This project closes that gap. The launcher is the Swarm service; it creates the daemon as a plain
container next to it, with one fixed security profile, and Swarm keeps owning the lifecycle:
updates, rollbacks, restarts and logs reach the daemon through the launcher.

## How it works

- **Image:** upstream `docker:29-dind-rootless` with its `rootless` user renumbered to the
  consumer's UID, plus the launcher binary as the entrypoint. Files that the daemon's containers
  write into the consumer's bind mounts come back owned by the consumer. One image per UID.
- **Launcher** (`launch`): an unprivileged Swarm service with only the host socket (non-root, all
  capabilities dropped, read-only, no route out). On start it takes a node-wide lock, checks the
  host paths and ranges with a short-lived probe container, refuses anything that conflicts with
  another instance, creates the daemon container from its own image digest, starts it, streams its
  logs and reports its health. On stop it stops and removes the daemon.
- **Fixed profile:** the daemon container always gets the same set (below), never
  `--privileged`, added capabilities beyond `SETUID` and `SETGID`, another device, a host
  namespace, the host socket or a host system path. The daemon listens on a Unix socket only, in a
  directory the consumer mounts read-only; there is no TCP listener.
- **One daemon per consumer**, never shared: each consumer is its own `--instance` with its own data
  root, socket directory and subordinate range.

[`docs/architecture.md`](docs/architecture.md) walks through the launcher's lifecycle, the node
lock, the probe and the ownership rules.

## Images

`ghcr.io/leinardi/swarm-rootless-docker`, for `linux/amd64`, one variant per UID:GID. Today: 568.

| Tag | Moves | Use |
| --- | --- | --- |
| `vX.Y.Z-uid568`, e.g. `v0.1.0-uid568` | never | pin an exact release; update by hand |
| `29-uid568` | to every new release on Docker 29 | follow updates (Gantry, a scheduled pull) |

There is no `latest`: the floating tag names the Docker major a consumer runs, and the next major
gets a floating tag of its own, which a consumer moves to deliberately.

A release is built daily when anything is releasable, including a **rebuild on a new upstream
digest** of `docker:29-dind-rootless` as a patch version: the image adds no package, so upstream's
fixes reach it only that way. A breaking change to the launcher's flags is a major version and
reaches the floating tag too; the release notes call out every flag change.
[`docs/release.md`](docs/release.md) has the details.

Every image is attested and signed by the release workflow. To check one:

```bash
gh attestation verify oci://ghcr.io/leinardi/swarm-rootless-docker:v0.1.0-uid568 \
  --repo leinardi/swarm-rootless-docker
cosign verify ghcr.io/leinardi/swarm-rootless-docker:v0.1.0-uid568 \
  --certificate-identity https://github.com/leinardi/swarm-rootless-docker/.github/workflows/release.yaml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Host requirements

- **Docker Engine** in Swarm mode, tested on Docker 29. The launcher speaks Docker API 1.53, so an
  older engine refuses it.
- **Unprivileged user namespaces.** On hosts that restrict them through AppArmor (Ubuntu 23.10
  and later, and GitHub's runners), rootlesskit fails with `fork/exec /proc/self/exe: operation
  not permitted` until `kernel.apparmor_restrict_unprivileged_userns=0`. That loosens the
  restriction for the whole host: weigh it, and persist it in `/etc/sysctl.d/` if you accept it.
  TrueNAS SCALE does not restrict them.
- **`/dev/net/tun`** on the host (the daemon's slirp4netns network needs it).
- **Host paths under root-owned parents.** The data root, the socket directory and every `--mount`
  source must sit under parents that are owned by root, not group- or other-writable, and carry no
  extended ACL (a ZFS dataset's NFSv4 ACL must be trivial). Only the leaf belongs to the consumer.
  No path may contain a symlink. The probe checks all of it and refuses otherwise: with anything
  less, the consumer or the daemon's subordinate UIDs could rename or replace a path between the
  check and its use.
- **A subordinate range per instance**, allocated by the host admin and recorded (the homelab
  repository, for example). See [Ranges](#ranges).

## Deploying an instance

[`deployments/docker/docker-compose.yaml`](deployments/docker/docker-compose.yaml) is a complete
stack: the launcher, its consumer and their networks. In short:

1. Create the host directories, owned by the image's UID with mode `0750`, under root-owned `0755`
   parents: the data root, the socket directory and every `--mount` source (here the workspace,
   which the consumer mounts too):

   ```bash
   install -d -o root -g root -m 0755 /mnt/pool/herdquay
   install -d -o 568 -g 568 -m 0750 /mnt/pool/herdquay/docker /mnt/pool/herdquay/docker-socket \
     /mnt/pool/herdquay/workspace
   ```

   A missing path fails the start: the probe refuses it, and Docker would not create it either.
   For containers that run as a non-root user inside the daemon, open the workspace with `0711`
   instead ([Users, files and permissions](#users-files-and-permissions)).

2. Pick a subordinate range no other instance, user or tool on the node uses, and record it.
3. Deploy the stack with the host socket's group:

   ```bash
   DOCKER_GID="$(stat -c %g /var/run/docker.sock)" \
     docker stack deploy -c deployments/docker/docker-compose.yaml herdquay-docker
   ```

The launcher service needs all of these, and the example sets them:

- `user: "65534:<group of /var/run/docker.sock>"`: non-root, in the socket's group. The image's
  default user is the daemon's UID, which must not reach the host socket.
- `cap_drop: [ALL]`, `read_only: true`, and tmpfs mounts at `/tmp` (the launcher's status socket)
  and at upstream's two volumes, `/home/rootless/.local/share/docker` and `/var/lib/docker`: an image
  cannot undeclare a `VOLUME`, so without the tmpfs mounts every task leaves two anonymous volumes
  behind.
- The host socket bind-mounted at `/var/run/docker.sock`, and an `internal: true` network: the
  launcher needs no route out. Swarm has no `none` network for services.
- `stop_grace_period: 40s`: a running launcher stops the daemon (15 s for dockerd to stop its own
  containers) and removes it within 30 s. A stop that arrives while the launcher is still
  starting waits for the start to end, which can take longer; if Swarm kills it first, the next
  launcher on the node removes what it left ([architecture](docs/architecture.md#the-launch)).
- `update_config.order: stop-first` and the same for `rollback_config`. `start-first` is not
  supported: two launchers for one data root would run at once, and the second is refused until
  the first is gone.
- A placement constraint on the node: the host paths and the range belong to one node.

The **consumer** mounts the socket directory read-only (a Unix socket needs no write access to its
mount), sets `DOCKER_HOST=unix://<mount>/docker.sock`, and mounts its workspace at the same path
the daemon sees it at (`--mount SOURCE:TARGET`), so that `docker run -v "$PWD:/src"` and Compose
bind mounts name a path the daemon can find. `docker info` then reports `rootless` among the
security options.

### Launch flags

Per-instance settings are flags of `launch`, and nothing else is: nothing about the daemon's
profile is configurable. A bad or repeated flag exits 2 and names the flag.

| Flag | Required | Rule |
| --- | --- | --- |
| `--instance NAME` | yes | 1 to 32 lowercase letters, digits and `-`, starting and ending with a letter or digit; the daemon container is `swarm-rootless-docker-daemon-NAME` |
| `--subuid START:COUNT` | yes | START at least 100000, COUNT at least 65536, ending below 4294967295 |
| `--subgid START:COUNT` | no | the same rules; defaults to the `--subuid` range |
| `--data-root PATH` | yes | the daemon's data root, owned by the image's UID |
| `--socket-dir PATH` | yes | the directory the daemon creates `docker.sock` in, owned by the image's UID |
| `--mount SOURCE:TARGET[:ro\|:rw]` | no, up to 16 | an extra bind mount, typically the workspace at the same path |
| `--network NAME` | yes | the network the daemon joins: its egress, and where its published ports answer; not `host` or `none` |
| `--memory SIZE` | yes | bytes, or with a `k`, `m` or `g` suffix; at least 256 MiB; also the swap limit |
| `--pids-limit N` | yes | 256 to 4194304 |
| `--cpus N` | no | above 0, at most 1024 (e.g. `2` or `1.5`) |
| `--log-level LEVEL` | no | the launcher's own: `error`, `warn`, `info` (default), `debug`, `trace` |
| `--log-format FORMAT` | no | the launcher's own, on stderr: `text` (default) or `json` |

Every path is absolute and clean (no `.`, `..`, empty component or trailing `/`, no `:`). No host
path may be, contain or lie under a host system path (`/bin`, `/boot`, `/dev`, `/etc`, `/lib*`,
`/proc`, `/root`, `/run`, `/sbin`, `/sys`, `/usr`, `/var/lib/docker`, `/var/run`). The data root,
the socket directory and the mount sources must not be the same path or nested, nor share storage
another way (a bind mount or a hard link of another path). A mount target may not be or lie under
the daemon's own paths (its system directories, `/home/rootless`, `/var/lib/docker`).

These lexical rules only catch mistakes early. **The boundary is the probe**: Docker resolves bind
sources on the host, which the launcher never sees, so before every start the launcher runs a probe
container from its own image, with the host's `/` mounted read-only, no network and no capability
but `DAC_READ_SEARCH`, which checks each path on the host itself: no symlink, root-owned and
non-writable ancestry, trivial ACLs, the right type and owner, no other filesystem mounted below,
and where each path really is, so that two paths reaching the same storage are caught.

### Exit codes and health

- The image's healthcheck is healthy only while the launcher runs and its daemon is running and
  healthy.
- The launcher exits with the daemon's exit code when the daemon stops on its own, 1 when the
  start is refused (a conflict, a probe refusal, the lock held for more than 30 s) or the daemon's
  log stream is lost, 0 after a clean stop, and 2 for a bad flag. Swarm restarts it, and a refused
  start is retried that way.
- An update or a rollback of the service restarts the daemon, which stops every container it runs.

## The daemon's profile

| Setting | Why |
| --- | --- |
| `CapDrop=ALL`, `CapAdd=SETUID,SETGID` | `newuidmap` and `newgidmap` work through file capabilities, which need these in the bounding set |
| no `no-new-privileges` | it would disable those file capabilities, and the daemon would not start |
| `seccomp=unconfined` | `unshare` and `mount`, for the user namespace and overlayfs |
| `apparmor=unconfined` | mounts inside the container |
| `MaskedPaths=[]`, `ReadonlyPaths=[]` | an unmasked `/proc` for the inner containers |
| `/dev/net/tun` | slirp4netns's tap device; without it the daemon exits |
| the image's UID and GID, private PID, IPC and cgroup namespaces, no port binding, `RestartPolicy=no` | nothing more than it needs; the launcher owns its lifecycle |

The daemon container's `/etc/subuid` and `/etc/subgid` are written by the launcher before it
starts, owned by root and out of the daemon user's reach: whoever controls those files controls
which host UIDs the daemon may map.

**What it still risks.** Unconfined seccomp and user namespaces expose more of the kernel to the
daemon's containers than a default container sees: a kernel bug reachable through them is a way
out. The profile defends against agent mistakes, malicious dependencies, prompt injection and a
compromised consumer reaching the host's Docker; it does not make the daemon's containers safe to
run hostile code against an unpatched kernel. Keep the host kernel updated.

## Users, files and permissions

Inside the daemon, its containers' root is the consumer's UID on the host (568), and an inner UID
`N` (1 and up) is host UID `START + N - 1` of the `--subuid` range. So:

- A container running as **root** (`-u 0:0`) writes the consumer's bind mounts as the consumer:
  this is the setting that "just works".
- A container running as **non-root** writes as a subordinate UID (`START + N - 1`), which the
  consumer does not own. It also needs to traverse the consumer's directories: a `0750` workspace
  stops it at the door; open the directory (`chmod 0711`) or run the container as root.
- **The `-u "$(id -u)"` trap:** in the consumer, `id -u` is 568, but inside the daemon UID 568 is
  host UID `START + 567`, not the consumer.
- **Recovery** from files a non-root container left behind, owned by subordinate UIDs that the
  consumer cannot delete: change them back through the daemon, as its root:

  ```bash
  docker run --rm -u 0:0 -v "$PWD:/work" alpine chown -R 0:0 /work
  ```

- On a ZFS dataset with NFSv4 ACLs, group inheritance can grant more than the mode bits say; the
  probe's trivial-ACL rule keeps that off the paths it checks, and files the daemon creates follow
  the dataset's inheritance.

### Ranges

The range must not contain a UID or GID of the host's `/etc/passwd` or `/etc/group`, nor overlap a
range in the host's `/etc/subuid` or `/etc/subgid` (Incus's `2147000001` block, for example), nor
another instance's range: the probe and the launcher refuse what they can see. They cannot see
rootless workloads that don't use the host's subordinate files, nor ranges used on other nodes:
node-wide uniqueness is the host admin's allocation. Give every instance a dedicated range and
record it.

## Limits

- **One limit for everything.** Without systemd, rootless mode runs with `Cgroup Driver: none`:
  `docker run --memory` and `--cpus` inside it are ignored. `--memory`, `--pids-limit` and `--cpus`
  on the launcher are the only limits, and the daemon and all its containers share them.
- **An inner OOM can kill dockerd.** When the containers fill `--memory`, the kernel may take
  dockerd itself. That is accepted: the launcher then exits non-zero, Swarm restarts it, and a new,
  healthy daemon comes back, at the cost of every container that was running. The host and other
  instances are not affected.
- **Published ports** answer at the daemon container's address on `--network`, not at the
  consumer's `localhost`.
- **No privileged containers** inside, so no Docker-in-Docker, no BPF, no host namespaces, and no
  kernel modules: what a test needs (`ifb`, `sch_netem`) must be loaded on the host.
- **Network:** the daemon's containers have the daemon's network reach. Limit its egress in the
  deployment if they should not reach the LAN.

## What CI does not cover

CI runs the unit tests, the image checks, and integration tests against a real rootless daemon on
a GitHub runner, Swarm included ([`docs/testing.md`](docs/testing.md)). It cannot reproduce the ZFS
NFSv4 ACL behaviour, group inheritance, the docker socket's group on the target node, Gantry, or a
multi-node swarm: check those on the node itself when deploying.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md). Security reports:
[SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE).
