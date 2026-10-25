# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
#
# The syntax line above pins the BuildKit frontend by index digest, so a moved 1.7 tag cannot
# change how this file is built. It is a parser directive and has to be the file's first line, ahead
# of this header. Dependabot's docker ecosystem does not update it: bump it by hand.
#
# The image is upstream's docker:<major>-dind-rootless with its rootless user renumbered to the
# consumer's UID and GID, and the launcher binary as the entrypoint. It adds no package. The
# runtime base is pinned by its floating major tag only: the release resolves the tag's digest
# once, builds from exactly that digest (a buildx build context overrides this FROM) and records
# it in the org.opencontainers.image.base.digest label. A local build uses the tag as is.

########################
# build: cross-compile a static musl binary for the target platform on the build platform, with
# cargo-zigbuild and zig as the C compiler and linker.
#
# The debian variant, because it carries the upstream toolchain, for which the upstream musl
# standard libraries below fit; the alpine variant's rustc is Alpine's own build for its
# x86_64-alpine-linux-musl host and cannot take another target's standard library.
########################
FROM --platform=$BUILDPLATFORM dhi.io/rust:1.98.1-debian13-dev AS build
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"

# The image's own toolchain version: the standard libraries added below must be the same release.
ARG RUST_VERSION=1.98.1
ARG ZIG_VERSION=0.17.0
ARG CARGO_ZIGBUILD_VERSION=0.24.0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# curl and xz fetch and unpack zig and the standard libraries; nothing from this stage but the
# binary ships. Unpinned: dhi.io republishes this tag with a refreshed package index, which would
# break any pinned version, and the download each tool makes is checked by its own sha256.
# droast ignore=DF005 reason="a pin breaks when dhi.io refreshes the tag's package index"
RUN apt-get update \
	&& apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
	&& rm -rf /var/lib/apt/lists/*

# zig for the build platform, checked against the sha256 ziglang.org publishes.
ARG BUILDARCH
RUN case "${BUILDARCH}" in \
		amd64) zig_arch=x86_64; zig_sha256=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;; \
		arm64) zig_arch=aarch64; zig_sha256=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;; \
		*) echo "no zig for build platform ${BUILDARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz" \
	&& echo "${zig_sha256}  /tmp/zig.tar.xz" | sha256sum -c - \
	&& mkdir /opt/zig \
	&& tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
	&& rm /tmp/zig.tar.xz
ENV PATH="/opt/zig:/root/.cargo/bin:${PATH}"

# From crates.io at the pinned version with its own lock file; cargo checks every crate's checksum.
RUN cargo install --locked "cargo-zigbuild@${CARGO_ZIGBUILD_VERSION}"

# The musl standard library of the target platform, checked against static.rust-lang.org's sha256.
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
		amd64) triple=x86_64-unknown-linux-musl; std_sha256=bd9f35880de21dab0e387e84009c00001ae7836a4ba565cb16823f13f75f325f ;; \
		arm64) triple=aarch64-unknown-linux-musl; std_sha256=7e0ad2e67817f224b7a6331e48d6397303fc050edaccf81784ed786ffddc884a ;; \
		*) echo "no Rust target for platform ${TARGETARCH}" >&2; exit 1 ;; \
	esac \
	&& curl -fsSL -o /tmp/rust-std.tar.xz "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION}-${triple}.tar.xz" \
	&& echo "${std_sha256}  /tmp/rust-std.tar.xz" | sha256sum -c - \
	&& mkdir /tmp/rust-std \
	&& tar -xJf /tmp/rust-std.tar.xz -C /tmp/rust-std --strip-components=1 \
	&& /tmp/rust-std/install.sh --prefix=/usr/local --disable-ldconfig \
	&& rm -rf /tmp/rust-std /tmp/rust-std.tar.xz \
	&& echo "${triple}" > /tmp/triple

WORKDIR /src
COPY . .

# The registry and the target directory are BuildKit caches, so the binary is copied out of them.
# The registry cache is shared by every platform's build, which may run in parallel: sharing=locked
# serialises them, since two cargo processes unpacking the same crate into it fail with
# "File exists". The target cache is per platform and needs no lock.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry,sharing=locked \
	--mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH} \
	triple="$(cat /tmp/triple)" \
	&& SWARM_ROOTLESS_DOCKER_VERSION="${VERSION}" \
		SWARM_ROOTLESS_DOCKER_COMMIT="${COMMIT}" \
		SWARM_ROOTLESS_DOCKER_DATE="${DATE}" \
		cargo zigbuild --release --locked --target "${triple}" --bin swarm-rootless-docker \
	&& mkdir -p /out \
	&& cp "target/${triple}/release/swarm-rootless-docker" /out/swarm-rootless-docker

########################
# runtime: upstream's rootless Docker, renumbered, with the launcher as the entrypoint.
#
# The daemon container runs this image as ROOTLESS_UID:ROOTLESS_GID, so the files the daemon
# creates in the consumer's data root and workspace belong to the consumer. Upstream's two VOLUMEs
# (/home/rootless/.local/share/docker and /var/lib/docker) cannot be undeclared here: the daemon
# spec covers both with mounts, and every container the launcher makes is removed with its
# anonymous volumes.
########################
FROM docker:29-dind-rootless AS runtime
ARG VERSION="dev"
ARG COMMIT="none"
ARG DATE="unknown"
# The consumer's UID and GID: one image variant per pair.
ARG ROOTLESS_UID=568
ARG ROOTLESS_GID=568
# The upstream index digest this image was built from; the release passes it.
ARG BASE_DIGEST="unpinned"

USER root
SHELL ["/bin/ash", "-eo", "pipefail", "-c"]

# Renumber the rootless user and group, and every file upstream gave them, refusing root, a
# non-number or an ID another entry already has. The subordinate ID files are emptied: the
# launcher writes the daemon's own, root-owned, into each daemon container before it starts, and
# the daemon refuses to start without them. newuidmap and newgidmap work through file
# capabilities, not setuid, and belong to root: nothing here touches them.
RUN uid="${ROOTLESS_UID}"; gid="${ROOTLESS_GID}"; \
	for id in "${uid}" "${gid}"; do \
		case "${id}" in \
			'' | *[!0-9]*) echo "ROOTLESS_UID and ROOTLESS_GID must be decimal IDs, got '${id}'" >&2; exit 1 ;; \
			*) ;; \
		esac; \
		if [ "${id}" -eq 0 ] || [ "${id}" -ge 4294967295 ]; then \
			echo "ROOTLESS_UID and ROOTLESS_GID must be from 1 to 4294967294, got ${id}" >&2; exit 1; \
		fi; \
	done; \
	old_uid="$(id -u rootless)"; old_gid="$(id -g rootless)"; \
	if awk -F: -v id="${uid}" '$1 != "rootless" && $3 == id { found = 1 } END { exit !found }' /etc/passwd; then \
		echo "UID ${uid} already belongs to another user" >&2; exit 1; \
	fi; \
	if awk -F: -v id="${gid}" '$1 != "rootless" && $3 == id { found = 1 } END { exit !found }' /etc/group; then \
		echo "GID ${gid} already belongs to another group" >&2; exit 1; \
	fi; \
	sed -i "s/^rootless:x:${old_uid}:${old_gid}:/rootless:x:${uid}:${gid}:/" /etc/passwd; \
	sed -i "s/^rootless:x:${old_gid}:/rootless:x:${gid}:/" /etc/group; \
	find / -xdev -user "${old_uid}" -exec chown -h "${uid}" {} +; \
	find / -xdev -group "${old_gid}" -exec chgrp -h "${gid}" {} +; \
	: > /etc/subuid; \
	: > /etc/subgid; \
	[ "$(id -u rootless)" = "${uid}" ] && [ "$(id -g rootless)" = "${gid}" ]

COPY --from=build /out/swarm-rootless-docker /usr/local/bin/swarm-rootless-docker

USER ${ROOTLESS_UID}:${ROOTLESS_GID}

# No TLS certificates: the daemon listens on its Unix socket only.
ENV DOCKER_TLS_CERTDIR=

# The seven OCI labels, the base this was built from, and the UID and GID the launcher reads to
# run the daemon as the consumer.
LABEL org.opencontainers.image.title="swarm-rootless-docker" \
	org.opencontainers.image.description="Launcher and image for a dedicated rootless Docker daemon per Docker Swarm service" \
	org.opencontainers.image.source="https://github.com/leinardi/swarm-rootless-docker" \
	org.opencontainers.image.version="${VERSION}" \
	org.opencontainers.image.revision="${COMMIT}" \
	org.opencontainers.image.created="${DATE}" \
	org.opencontainers.image.licenses="MIT" \
	org.opencontainers.image.base.name="docker.io/library/docker:29-dind-rootless" \
	org.opencontainers.image.base.digest="${BASE_DIGEST}" \
	io.github.leinardi.swarm-rootless-docker.uid="${ROOTLESS_UID}" \
	io.github.leinardi.swarm-rootless-docker.gid="${ROOTLESS_GID}"

# The launcher container's health: its daemon runs and is healthy. The start period covers a wait
# for the node lock, the probe and the daemon's own start period.
HEALTHCHECK --interval=10s --timeout=5s --start-period=120s --retries=3 \
	CMD ["/usr/local/bin/swarm-rootless-docker", "healthcheck"]

# Setting the entrypoint also clears upstream's CMD: no argument reaches the binary unless the
# service gives one, and with none it prints its usage and exits 2.
ENTRYPOINT ["/usr/local/bin/swarm-rootless-docker"]
