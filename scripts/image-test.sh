#!/usr/bin/env bash
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
# Checks a built image without starting a daemon: what the Dockerfile promises about the user,
# its files, the labels, the entrypoint and the healthcheck.
#
#   scripts/image-test.sh IMAGE UID GID
#
# Every check runs; the script exits 1 if any failed, naming each.

set -euo pipefail

if [[ $# -ne 3 ]]; then
	echo "usage: $0 IMAGE UID GID" >&2
	exit 2
fi
image="$1"
uid="$2"
gid="$3"
binary=/usr/local/bin/swarm-rootless-docker
failures=0

pass() {
	echo "ok   $1"
}

fail() {
	echo "FAIL $1" >&2
	failures=$((failures + 1))
}

# expect NAME WANT GOT
expect() {
	if [[ "$2" == "$3" ]]; then
		pass "$1"
	else
		fail "$1: want '$2', got '$3'"
	fi
}

# In the image, as root, with a shell: for reading what the image holds.
inspect_files() {
	docker run --rm --user 0:0 --network none --entrypoint /bin/sh "${image}" -c "$1"
}

label() {
	docker image inspect "${image}" --format "{{ index .Config.Labels \"$1\" }}"
}

expect "default user" "${uid}:${gid}" "$(docker image inspect "${image}" --format '{{.Config.User}}')"
expect "uid label" "${uid}" "$(label io.github.leinardi.swarm-rootless-docker.uid)"
expect "gid label" "${gid}" "$(label io.github.leinardi.swarm-rootless-docker.gid)"
expect "base name label" "docker.io/library/docker:29-dind-rootless" \
	"$(label org.opencontainers.image.base.name)"
expect "entrypoint" "[${binary}]" "$(docker image inspect "${image}" --format '{{.Config.Entrypoint}}')"
expect "no command" "[]" "$(docker image inspect "${image}" --format '{{.Config.Cmd}}')"
expect "healthcheck" "[CMD ${binary} healthcheck]" \
	"$(docker image inspect "${image}" --format '{{.Config.Healthcheck.Test}}')"
expect "no TLS certificate directory" "DOCKER_TLS_CERTDIR=" \
	"$(docker image inspect "${image}" --format '{{range .Config.Env}}{{println .}}{{end}}' |
		grep '^DOCKER_TLS_CERTDIR=' || true)"

expect "rootless user" "rootless:x:${uid}:${gid}:Rootless:/home/rootless:/bin/sh" \
	"$(inspect_files 'grep "^rootless:" /etc/passwd')"
expect "rootless group" "rootless:x:${gid}:" "$(inspect_files 'grep "^rootless:" /etc/group')"
expect "process identity" "uid=${uid}(rootless) gid=${gid}(rootless) groups=${gid}(rootless)" \
	"$(docker run --rm --network none --entrypoint id "${image}")"
expect "empty subordinate ID files" "0 0" "$(inspect_files 'stat -c %s /etc/subuid /etc/subgid | xargs')"
expect "home owned by the user" "${uid}:${gid}" \
	"$(inspect_files 'stat -c %u:%g /home/rootless /home/rootless/.local/share/docker | sort -u')"
# Upstream's user is 1000:1000; for that variant there is nothing to renumber.
if [[ "${uid}" != 1000 && "${gid}" != 1000 ]]; then
	expect "nothing left with upstream's IDs" "" \
		"$(inspect_files 'find / -xdev \( -user 1000 -o -group 1000 \) -print')"
fi
expect "the ID maps belong to root" "0:0 0:0" \
	"$(inspect_files 'stat -c %u:%g /usr/bin/newuidmap /usr/bin/newgidmap | xargs')"

# The binary itself: its version, and its usage (exit 2) for no argument or for the upstream
# entrypoint's arguments, which must never start a daemon.
if docker run --rm --network none "${image}" --version | grep -q '^swarm-rootless-docker '; then
	pass "version"
else
	fail "version"
fi
for args in "" "dockerd" "--debug" "daemon --debug"; do
	read -ra argv <<< "${args}"
	status=0
	docker run --rm --network none "${image}" "${argv[@]}" > /dev/null 2>&1 || status=$?
	expect "usage for '${args}'" 2 "${status}"
done

if [[ "${failures}" -ne 0 ]]; then
	echo "${failures} check(s) failed" >&2
	exit 1
fi
echo "every check passed"
