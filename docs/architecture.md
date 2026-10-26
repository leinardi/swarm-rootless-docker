# Architecture

How the launcher creates, supervises and removes its daemon, and the rules that keep two launchers
on one node, or a launcher and a container it does not own, from getting in each other's way. The
[README](../README.md) is the user-facing contract; this is how it is kept.

## One binary, five modes

| Subcommand | Runs in | Does |
| --- | --- | --- |
| `launch` | the Swarm service's container | everything below |
| `probe` | the probe container | checks the host paths and ranges of one request, prints one JSON report line |
| `daemon` | the daemon container | checks its UID and subordinate files, then execs upstream's entrypoint with `dockerd --host=unix:///home/rootless/socket/docker.sock` and nothing else |
| `daemon-health` | the daemon container's healthcheck | pings dockerd on its socket |
| `healthcheck` | the launcher container's healthcheck | asks the launcher's status socket |

Only `launch` is in the help. No argument, or upstream's own arguments (`dockerd`, `--debug`),
print the usage and exit 2: nothing reaches the upstream entrypoint's default path, which would add
a TCP listener.

## The launch

1. **Identify itself.** The launcher's own container ID from `/proc/self/mountinfo`, then its image
   ID by inspecting itself, then the daemon's UID and GID from that image's labels. The daemon and
   the probe run from exactly this image ID, never a tag.
2. **Take the node lock.** Create, and never start, a container named
   `swarm-rootless-docker-lock`, labelled with the launcher's ID. Container names are unique per
   daemon, so the create is an atomic test-and-set, and a conflict means the lock is held:
   - the holder's launcher is gone or not running: the lock is stale (a crash); remove it by ID and
     retry;
   - the holder is running: retry with jittered backoff for up to 30 s, then exit 1 and let Swarm
     retry;
   - anything else (an inspect error, a container of that name without the labels): refuse.
3. **Under the lock, list every daemon** of every instance, always with `all=true`: a daemon is a
   reservation in any state, created-but-not-started included, which is exactly what another
   launcher leaves between its create and its start. For this instance's own name:
   - a container without the launcher's labels: refuse, and never touch it;
   - owned by this launcher, or by a launcher that is gone or not running: remove it (by ID, with
     its anonymous volumes);
   - owned by a running launcher, or unknown: refuse (exit 1).
4. **Under the lock, probe.** Run the probe container for this instance's paths and ranges, with
   the host's paths that are always denied (Docker's data root and the source of the launcher's own
   socket mount). Its report must be exactly one line that agrees with its exit code.
5. **Under the lock, check conflicts** against every listed daemon, from their labels: an
   overlapping subordinate range, or a path that is the same, nested, or reaches the same storage
   (by host location, not only by name).
6. **Under the lock, create the daemon** with the fixed spec and labels that record the instance,
   the ranges and each path's host location. Then release the lock. If the release fails, the
   daemon is removed: an unreleased lock with a daemon behind it would block the node.
7. **Start.** Upload `/etc/subuid` and `/etc/subgid` (root-owned, `0644`) into the created
   container, attach to its output, start it, and only then register the wait: Docker's
   `not-running` wait answers at once for a created container, and with `AutoRemove=false` a late
   wait still gets the real exit code.
8. **Supervise.** Until one of: the daemon exits (the launcher exits with its code, never mapping a
   failure to 0), the log stream fails (stop, exit 1), or a stop signal arrives. Every 10 s the
   daemon's health is inspected and published on the status socket that the image's healthcheck
   asks.
9. **Stop.** One budget of 30 s in two parts: 20 s for the stop by ID (dockerd's 15 s to stop its
   own containers, then the kill) and the exit report, then 10 s in which the removal and the log
   drain run together, so a slow drain cannot use up the removal's time.

The start (steps 2 to 6) is never cancelled: a stop signal only sets a flag, checked once the lock
is released. If it is set, the created daemon is removed and the launcher exits 0. Every Docker
call is bounded as a whole (10 s) and the probe has its own 20 s, so the start always ends, but
**the start as a whole has no budget**: a lock wait of up to 30 s, then a probe of up to 20 s and
the calls around it, can outlast the service's 40 s stop grace period. A stop that arrives during
the start is therefore not guaranteed a clean exit: Swarm may kill the launcher first, leaving
behind its lock, its probe or its created daemon.

Nothing left that way blocks the node for good. Each carries the dead launcher's ID, so the next
launcher on the node removes it under the stale rules below: the lock when its owner is not
running, and the probe and the daemon in step 3. A crash at any point leaves the same, and is
recovered the same way. Guaranteeing the cleanup itself would take one deadline for the whole
start, shared by the lock wait, the probe and every call, and shorter than the grace period.

## Ownership

The launcher only ever removes or stops a container it provably owns, and always by ID, never by
name:

- **The lock and the daemon** carry `io.github.leinardi.swarm-rootless-docker.role` and
  `.launcher=<launcher container ID>`. A same-name container without them is foreign and refused.
- **Stale** means the owning launcher is not found or not running; a running owner, an inspect
  error or a missing state are never stale.
- The probe is created, waited for and removed by its ID on every path, also when it times out.

## The probe

The lexical flag checks cannot be the boundary: Docker resolves bind sources on the host, which the
launcher never sees. The probe sees it, read-only: the host's `/` at `/host` with
`ReadOnlyForceRecursive` (Docker then picks `rslave`, or private when `/` is private), network
`none`, read-only root, all capabilities dropped but `DAC_READ_SEARCH` (to walk `0750`
directories), `no-new-privileges`, uid 0, 64 MiB and 64 pids. It reads metadata only, through
`openat2` with `RESOLVE_IN_ROOT | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`, and the host's
`/etc/passwd`, `/etc/group`, `/etc/subuid` and `/etc/subgid`. It refuses:

- a symlink in any component of a path;
- an ancestor that is not root-owned, is group- or other-writable, or carries an extended ACL: a
  POSIX ACL at all, or a TrueNAS NFSv4 ACL (`system.nfs4_acl_xdr`) that is not trivial (only
  `owner@`, `group@` and `everyone@` entries matching the mode, no inheritance flags, no named user
  or group). An ACL it cannot decode is refused too;
- a leaf of the wrong type or owner, or a data root or socket directory that is group- or
  other-writable;
- a path that is, contains or lies under a host system path or a denied path, each as written and
  with its symlinks followed, and including every filesystem mounted at or below it;
- a path with another filesystem mounted below it, a mount whose root is deleted, or a mount stack
  with no single top;
- a range containing a host user or group, or overlapping a host subordinate range.

Each path is placed with the probe's own mount table, resolving which mount is visible at each
prefix as the kernel does, so a bind mount that shows one directory under another name is seen for
what it is. That location (the filesystem's `major:minor`, the position in it, and the file's
device and inode) goes into the daemon's labels, which is how step 5 compares instances. Device
numbers are per boot; a daemon never outlives its launcher across a reboot, so labels and probes
always share one.

A mount created by host root after the probe is out of scope: host root is trusted.

## The daemon spec

One function builds it, and a unit test pins it whole: the launcher's own image ID, the image's
UID and GID, the [profile in the README](../README.md#the-daemons-profile), `Mounts` (never `Binds`,
so a missing source fails the create), tmpfs at `/var/lib/docker` and `/run/user`, the resources
from the flags (swap equal to memory), a `local` log driver (10 MB × 3), a 15 s stop timeout,
`RestartPolicy=no` and `AutoRemove=false`.
