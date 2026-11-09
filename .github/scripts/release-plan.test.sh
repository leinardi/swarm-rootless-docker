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

# Tests for release-plan.sh, against scratch git repositories. Plain bash: stub gh, docker and svu
# commands, first on PATH, answer from files in the scratch directory:
#
#   ci        "<sha> <state>", what require-ci-green said about a commit (green, or red, pending
#             or missing: not green); run_plan passes HEAD's as HEAD_CI, and the script itself
#             never reads CI (release.yaml's ci-green job gates the planned commit)
#   releases  "<tag> published|draft|error", the GitHub releases (a tag with no line has none)
#   images    "<reference> <value>": for an upstream reference its index digest, for a published
#             version its base digest label, "error", or "config-error" (the index reads, its
#             config does not); a reference with no line is not found
#   svu_next  what svu next prints (no file: the version it counts from, so nothing to bump)
#
# Run: bash .github/scripts/release-plan.test.sh

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="${here}/release-plan.sh"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

image_repo=ghcr.io/leinardi/example
d29a="sha256:$(printf 'a%.0s' {1..64})"
d29b="sha256:$(printf 'b%.0s' {1..64})"
d30="sha256:$(printf 'c%.0s' {1..64})"

# Every git command here, the script's included, sees only the scratch repositories.
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid

mkdir "${work}/bin"

cat >"${work}/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
lookup() {
	awk -v key="$2" '$1 == key { print $2; found = 1 } END { exit !found }' "${STUB_DIR}/$1" 2>/dev/null
}
case "$1" in
release)
	[[ "$2" == view ]] || { echo "stub gh: unexpected release call $*" >&2; exit 99; }
	case "$(lookup releases "$3" || echo absent)" in
	published) echo false ;;
	draft) echo true ;;
	absent) echo "release not found" >&2; exit 1 ;;
	*) echo "HTTP 502" >&2; exit 1 ;;
	esac
	;;
*) echo "stub gh: unexpected call $*" >&2; exit 99 ;;
esac
STUB

cat >"${work}/bin/docker" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2 $3 $5" == "buildx imagetools inspect --format" ]] || { echo "stub docker: unexpected call $*" >&2; exit 99; }
value="$(awk -v key="$4" '$1 == key { print $2 }' "${STUB_DIR}/images" 2>/dev/null)"
case "${value}" in
"") echo "ERROR: $4: not found" >&2; exit 1 ;;
error) echo "ERROR: failed to authorize: 403 Forbidden" >&2; exit 1 ;;
config-error)
	[[ "$6" == '{{json .Manifest}}' ]] || { echo "ERROR: failed to read the config" >&2; exit 1; }
	value="sha256:0000000000000000000000000000000000000000000000000000000000000000"
	;;
*) ;;
esac
case "$6" in
'{{json .Manifest}}') echo "{\"digest\":\"${value}\"}" ;;
'{{json .Image}}') echo "{\"config\":{\"Labels\":{\"org.opencontainers.image.base.digest\":\"${value}\"}}}" ;;
*) echo "stub docker: unexpected format $6" >&2; exit 99 ;;
esac
STUB

cat >"${work}/bin/svu" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == "next --tag.pattern" && $# -eq 3 ]] || { echo "stub svu: unexpected call $*" >&2; exit 99; }
if [[ -f "${STUB_DIR}/svu_next" ]]; then cat "${STUB_DIR}/svu_next"; else echo "$3"; fi
STUB
chmod +x "${work}/bin/"*

failures=0
case_count=0

# A fresh repository and stub state for one case; its first commit's Dockerfile is on Docker 29.
new_repo() {
	case_count=$((case_count + 1))
	repo="${work}/case${case_count}"
	export STUB_DIR="${repo}.stubs"
	mkdir -p "${repo}/deployments/docker" "${STUB_DIR}"
	: >"${STUB_DIR}/ci"
	: >"${STUB_DIR}/releases"
	: >"${STUB_DIR}/images"
	git -C "${repo}" init -q -b main
	dockerfile 29
	commit "feat(launcher): first" >/dev/null
}

# dockerfile MAJOR [legacy]: the runtime base in its canonical spelling, or as v0.3.0 and earlier
# write it (docker:<major>-dind-rootless).
dockerfile() {
	local base="docker.io/library/docker"
	if [[ "${2:-}" == legacy ]]; then
		base=docker
	fi
	printf 'FROM dhi.io/rust:1 AS build\nFROM %s:%s-dind-rootless AS runtime\n' "${base}" "$1" >"${repo}/deployments/docker/Dockerfile"
}

commit() {
	echo "$1" >>"${repo}/log"
	git -C "${repo}" add -A
	git -C "${repo}" commit -q -m "$1"
	git -C "${repo}" rev-parse HEAD
}

head_sha() {
	git -C "${repo}" rev-parse HEAD
}

tag() {
	git -C "${repo}" tag -a "$1" -m "$1" "${2:-HEAD}"
}

# tag_recorded VERSION COMMIT BASE_DIGEST BASELINE: the tag as release.yaml creates it, its
# annotation carrying a toolchain line and the record subcommand's lines.
tag_recorded() {
	local lines
	lines="$(env -i PATH="${PATH}" bash "${script}" record docker.io/library/docker:29-dind-rootless "$3" "$4")"
	git -C "${repo}" tag -a "$1" -m "$(printf 'swarm-rootless-docker %s\n\nRust: 1.98.1\n%s' "$1" "${lines}")" "$2"
}

ci() {
	sed -i "/^$1 /d" "${STUB_DIR}/ci"
	echo "$1 $2" >>"${STUB_DIR}/ci"
}

# A published, finished release of $1 for the 568 variant, built on base digest $2.
published() {
	echo "$1 published" >>"${STUB_DIR}/releases"
	echo "${image_repo}:$1-uid568 $2" >>"${STUB_DIR}/images"
}

upstream() {
	echo "docker.io/library/docker:$1-dind-rootless $2" >>"${STUB_DIR}/images"
}

# run_plan EVENT [VERSION]: runs the plan; leaves status, out (the outputs) and err.
run_plan() {
	out_file="${STUB_DIR}/output"
	: >"${out_file}"
	local head_ci
	if [[ -v HEAD_CI_SET ]]; then
		head_ci="${HEAD_CI_SET}"
	elif [[ "$(awk -v key="$(head_sha)" '$1 == key { print $2 }' "${STUB_DIR}/ci")" == green ]]; then
		head_ci=green
	else
		head_ci=not-green
	fi
	status=0
	err="$(cd "${repo}" && env -i PATH="${work}/bin:${PATH}" HOME="${work}" STUB_DIR="${STUB_DIR}" \
		GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
		EVENT="$1" VERSION_INPUT="${2:-}" HEAD_CI="${head_ci}" REPOSITORY=leinardi/example IMAGE_REPO="${image_repo}" \
		IMAGE_VARIANTS="${VARIANTS:-568:568}" RELEASE_TAG_REGEX='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
		GITHUB_OUTPUT="${out_file}" GITHUB_STEP_SUMMARY="${STUB_DIR}/summary" \
		bash "${script}" plan 2>&1)" || status=$?
	out="$(cat "${out_file}")"
}

pass() {
	echo "ok   $1"
}

fail() {
	echo "FAIL $1" >&2
	failures=$((failures + 1))
}

# expect_plan NAME KEY=VALUE...: the plan succeeded with exactly these values (keys not named
# must be empty).
expect_plan() {
	local name="$1" key want got pair problems=""
	shift
	if [[ "${status}" -ne 0 ]]; then
		fail "${name}: exit ${status}: ${err}"
		return
	fi
	local -A given=()
	for pair in "$@"; do
		given["${pair%%=*}"]="${pair#*=}"
	done
	# Unless a case names them: the runtime FROM is the canonical base_ref, and only a base
	# rebuild scans against its base digest.
	if [[ ! -v given[base_from] ]]; then
		given[base_from]="${given[base_ref]:-}"
	fi
	if [[ ! -v given[base_repo] && -n "${given[base_from]}" ]]; then
		given[base_repo]="${given[base_from]%:*}"
	fi
	if [[ ! -v given[scan_baseline] && "${given[mode]:-}" == base ]]; then
		given[scan_baseline]="docker.io/library/docker@${given[base_digest]}"
	fi
	for key in mode sha version previous major base_ref base_from base_repo base_digest full_suite scan_baseline; do
		want="${given[${key}]:-}"
		got="$(sed -n "s/^${key}=//p" <<<"${out}")"
		if [[ "${got}" != "${want}" ]]; then
			problems+=" ${key}='${got}' want '${want}';"
		fi
	done
	if [[ -n "${problems}" ]]; then
		fail "${name}:${problems} (${err})"
	else
		pass "${name}"
	fi
}

# expect_refusal NAME MESSAGE: the plan failed, wrote no outputs and said MESSAGE.
expect_refusal() {
	if [[ "${status}" -eq 0 ]]; then
		fail "$1: succeeded with ${out//$'\n'/ }"
	elif [[ -n "${out}" ]]; then
		fail "$1: failed but wrote outputs ${out//$'\n'/ }"
	elif ! grep -qF -- "$2" <<<"${err}"; then
		fail "$1: want '$2' in: ${err}"
	else
		pass "$1"
	fi
}

expect_summary() {
	if grep -qF -- "$2" "${STUB_DIR}/summary" 2>/dev/null; then
		pass "$1"
	else
		fail "$1: want '$2' in the job summary"
	fi
}

# --- head releases -------------------------------------------------------------------------------

new_repo
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29a}"
head="$(commit "feat(launcher): second")"
ci "${head}" green
echo v0.2.0 >"${STUB_DIR}/svu_next"
run_plan schedule
expect_plan "schedule: releasable commits with green CI release HEAD" mode=head sha="${head}" version=v0.2.0 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=false

run_plan workflow_dispatch
expect_plan "dispatch: releasable commits release HEAD" mode=head sha="${head}" version=v0.2.0 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=false

run_plan workflow_dispatch 1.0.0
expect_plan "dispatch: an explicit higher version releases HEAD" mode=head sha="${head}" version=v1.0.0 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=false

run_plan workflow_dispatch v0.0.9
expect_refusal "dispatch: an explicit lower version is refused" "is not higher than the latest release v0.1.0"

run_plan workflow_dispatch 01.2.3
expect_refusal "dispatch: a malformed version is refused" "no leading zeros"

run_plan schedule 1.0.0
expect_refusal "schedule: a version input is refused" "only a dispatch takes a version"

# A base digest the previous release was not built on has had no release's whole suite yet.
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${d29b}#" "${STUB_DIR}/images"
run_plan schedule
expect_plan "schedule: HEAD on a base the previous release was not built on runs the full suite" mode=head \
	sha="${head}" version=v0.2.0 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless \
	base_digest="${d29b}" full_suite=true
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${d29a}#" "${STUB_DIR}/images"

# A version from svu that is not a release tag, or not higher, is refused.
echo v0.2.0-rc.1 >"${STUB_DIR}/svu_next"
run_plan schedule
expect_refusal "svu: a pre-release is refused" "which is not a vX.Y.Z tag"
echo v0.0.1 >"${STUB_DIR}/svu_next"
run_plan schedule
expect_refusal "svu: a lower version is refused" "which is not higher than v0.1.0"

# --- the first release ---------------------------------------------------------------------------

new_repo
head="$(head_sha)"
ci "${head}" green
upstream 29 "${d29a}"
run_plan workflow_dispatch
expect_refusal "dispatch: the first release needs a version" "the first release needs an explicit version"

run_plan workflow_dispatch 0.1.0
expect_plan "dispatch: the first release with a version" mode=head sha="${head}" version=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true

run_plan schedule
expect_plan "schedule: nothing before the first release" mode=none
expect_summary "schedule: the summary says the first release is dispatched" "the first one is dispatched"

# Tags that are not releases are not the latest release.
tag v1.0.0-rc.1
tag v01.2.3
run_plan schedule
expect_plan "schedule: pre-release and malformed tags are not releases" mode=none

# --- CI on HEAD ----------------------------------------------------------------------------------

# Only a schedule reads it, as HEAD_CI: whether HEAD is released or only a base rebuild is
# considered. A dispatch releases HEAD whatever CI says so far: the ci-green job waits for it.
new_repo
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29a}"
head="$(commit "fix(launcher): second")"
echo v0.1.1 >"${STUB_DIR}/svu_next"
for state in red pending missing; do
	ci "${head}" "${state}"
	run_plan schedule
	expect_plan "schedule: CI ${state} on HEAD with an unchanged base is a no-op" mode=none
	expect_summary "schedule: CI ${state} on HEAD is warned about" "but ci-result is not green on HEAD"
	run_plan workflow_dispatch
	expect_plan "dispatch: CI ${state} so far still plans HEAD" mode=head sha="${head}" version=v0.1.1 \
		previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=false
	run_plan workflow_dispatch 0.2.0
	expect_plan "dispatch: CI ${state} so far still plans an explicit version" mode=head sha="${head}" \
		version=v0.2.0 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" \
		full_suite=false
done
for value in "" maybe success; do
	HEAD_CI_SET="${value}" run_plan schedule
	expect_refusal "schedule: HEAD_CI '${value}' is refused" "HEAD_CI must be green or not-green"
done
HEAD_CI_SET="" run_plan workflow_dispatch
expect_plan "dispatch: HEAD_CI is not read" mode=head sha="${head}" version=v0.1.1 previous=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=false

# --- base rebuilds -------------------------------------------------------------------------------

new_repo
release_sha="$(head_sha)"
tag v0.1.0
ci "${release_sha}" green
published v0.1.0 "${d29a}"
upstream 29 "${d29b}"
run_plan schedule
expect_plan "schedule: a new upstream digest rebuilds the release" mode=base sha="${release_sha}" version=v0.1.1 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true

run_plan workflow_dispatch
expect_refusal "dispatch: a new upstream digest alone is nothing to bump" "nothing to bump"

# Docs-only commits since the release do not change what is rebuilt.
head="$(commit "docs(readme): words")"
run_plan schedule
expect_plan "schedule: the rebuild is of the release's commit, not HEAD" mode=base sha="${release_sha}" \
	version=v0.1.1 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" \
	full_suite=true

# Releasable commits with red CI wait; the base rebuild still happens.
head="$(commit "feat(launcher): unreviewed")"
ci "${head}" red
echo v0.2.0 >"${STUB_DIR}/svu_next"
run_plan schedule
expect_plan "schedule: red CI with a new base rebuilds the release" mode=base sha="${release_sha}" version=v0.1.1 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true
expect_summary "schedule: the skipped commits are warned about" "would release v0.2.0, but ci-result is not green"

# Docker 30 on HEAD, unreleased: the rebuild stays on the release's Docker 29.
dockerfile 30
head="$(commit "feat(deps): docker 30")"
ci "${head}" red
upstream 30 "${d30}"
run_plan schedule
expect_plan "schedule: 29 to 30, the rebuild stays on the release's 29" mode=base sha="${release_sha}" \
	version=v0.1.1 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" \
	full_suite=true

ci "${head}" green
run_plan schedule
expect_plan "schedule: 29 to 30, a green HEAD releases on 30" mode=head sha="${head}" version=v0.2.0 \
	previous=v0.1.0 major=30 base_ref=docker.io/library/docker:30-dind-rootless base_digest="${d30}" full_suite=true

# Every variant is compared: one on an old base is enough.
new_repo
release_sha="$(head_sha)"
tag v0.1.0
ci "${release_sha}" green
published v0.1.0 "${d29b}"
echo "${image_repo}:v0.1.0-uid1000 ${d29a}" >>"${STUB_DIR}/images"
upstream 29 "${d29b}"
VARIANTS="568:568 1000:1000" run_plan schedule
expect_plan "schedule: one variant on an old base rebuilds" mode=base sha="${release_sha}" version=v0.1.1 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true
run_plan schedule
expect_plan "schedule: the current base is a no-op" mode=none

VARIANTS="568:568 568:1000" run_plan schedule
expect_refusal "variants: one UID twice is refused" "names UID 568 twice"
VARIANTS="0:0" run_plan schedule
expect_refusal "variants: root is refused" "is not UID:GID"
VARIANTS=" " run_plan schedule
expect_refusal "variants: none is refused" "IMAGE_VARIANTS is empty"

# Lookups fail closed.
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless error#" "${STUB_DIR}/images"
run_plan schedule
expect_refusal "schedule: an upstream lookup error stops the run" "could not inspect docker.io/library/docker:29-dind-rootless"
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless not-a-digest#" "${STUB_DIR}/images"
run_plan schedule
expect_refusal "schedule: an upstream digest that is not one stops the run" "has no index digest"
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${d29b}#" "${STUB_DIR}/images"
sed -i "s#^${image_repo}:v0.1.0-uid568 .*#${image_repo}:v0.1.0-uid568 error#" "${STUB_DIR}/images"
run_plan schedule
expect_refusal "schedule: a GHCR lookup error stops the run" "could not inspect ${image_repo}:v0.1.0-uid568"

# A runtime stage that is not docker:<major>-dind-rootless.
for from in "docker:29-dind-rootless@${d29a}" "docker:dind-rootless" "docker:29-dind" \
	"docker.io/library/docker:29-dind-rootless@${d29a}" "docker.io/docker:29-dind-rootless" \
	"library/docker:29-dind-rootless" "index.docker.io/library/docker:29-dind-rootless" \
	"docker.io/library/docker:dind-rootless"; do
	new_repo
	printf 'FROM dhi.io/rust:1 AS build\nFROM %s AS runtime\n' "${from}" >"${repo}/deployments/docker/Dockerfile"
	head="$(commit "feat(image): odd base")"
	ci "${head}" green
	run_plan workflow_dispatch 0.1.0
	expect_refusal "dockerfile: FROM ${from} is refused" "want docker.io/library/docker:<major>-dind-rootless"
done
new_repo
printf 'FROM docker:29-dind-rootless AS build\nFROM docker:29-dind-rootless AS runtime\nFROM docker:29-dind-rootless AS runtime\n' \
	>"${repo}/deployments/docker/Dockerfile"
head="$(commit "feat(image): two runtimes")"
ci "${head}" green
run_plan workflow_dispatch 0.1.0
expect_refusal "dockerfile: two runtime stages are refused" "exactly one stage named runtime"

# --- recovery ------------------------------------------------------------------------------------

new_repo
release_sha="$(head_sha)"
tag v0.1.0
upstream 29 "${d29a}"
head="$(commit "docs(readme): words")"
run_plan schedule
expect_plan "schedule: an unpublished latest release is finished" mode=recover sha="${release_sha}" version=v0.1.0 \
	major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true
run_plan workflow_dispatch
expect_plan "dispatch: an unpublished latest release is finished" mode=recover sha="${release_sha}" version=v0.1.0 \
	major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true

echo "${image_repo}:v0.1.0-uid568 ${d29a}" >>"${STUB_DIR}/images"
run_plan schedule
expect_plan "schedule: a release with no GitHub release is finished" mode=recover sha="${release_sha}" \
	version=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true
echo "v0.1.0 draft" >>"${STUB_DIR}/releases"
run_plan schedule
expect_plan "schedule: a draft release is finished" mode=recover sha="${release_sha}" version=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true
# The base a recovery builds on, and names, is the one its published images record.
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${d29b}#" "${STUB_DIR}/images"
run_plan schedule
expect_plan "schedule: a recovery keeps the base its images record" mode=recover sha="${release_sha}" \
	version=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true
VARIANTS="568:568 1000:1000" run_plan schedule
expect_plan "schedule: a recovery builds the missing variant on the recorded base" mode=recover \
	sha="${release_sha}" version=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" \
	full_suite=true
echo "${image_repo}:v0.1.0-uid1000 ${d29b}" >>"${STUB_DIR}/images"
VARIANTS="568:568 1000:1000" run_plan schedule
expect_refusal "schedule: variants on different bases are refused" "record the bases '${d29a} ${d29b}'"
sed -i "/-uid1000 /d" "${STUB_DIR}/images"
sed -i "s#^${image_repo}:v0.1.0-uid568 .*#${image_repo}:v0.1.0-uid568 config-error#" "${STUB_DIR}/images"
run_plan schedule
expect_refusal "schedule: a recorded base that cannot be read stops the run" "could not inspect ${image_repo}:v0.1.0-uid568"
sed -i "s#^${image_repo}:v0.1.0-uid568 .*#${image_repo}:v0.1.0-uid568 unpinned#" "${STUB_DIR}/images"
run_plan schedule
expect_refusal "schedule: a recorded base that is not a digest is refused" "record the bases 'unpinned'"
sed -i "s#^${image_repo}:v0.1.0-uid568 .*#${image_repo}:v0.1.0-uid568 ${d29a}#" "${STUB_DIR}/images"
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${d29a}#" "${STUB_DIR}/images"

sed -i 's/^v0.1.0 draft$/v0.1.0 error/' "${STUB_DIR}/releases"
run_plan schedule
expect_refusal "schedule: an unreadable GitHub release stops the run" "could not read the release v0.1.0"
sed -i 's/^v0.1.0 error$/v0.1.0 published/' "${STUB_DIR}/releases"
run_plan schedule
expect_plan "schedule: a finished release is not recovered" mode=none

# An explicit existing version is recovered from its own commit, wherever HEAD is.
second_sha="$(commit "fix(launcher): second")"
tag v0.1.1
published v0.1.1 "${d29a}"
commit "docs(readme): more" >/dev/null
run_plan workflow_dispatch 0.1.0
expect_plan "dispatch: an explicit released version is recovered from its commit" mode=recover sha="${release_sha}" \
	version=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" full_suite=true
run_plan workflow_dispatch v0.1.1
expect_plan "dispatch: the previous version of a recovery is the one below it" mode=recover sha="${second_sha}" \
	version=v0.1.1 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" \
	full_suite=false

# A latest release off HEAD's history is refused.
new_repo
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29a}"
git -C "${repo}" checkout -q -b side
commit "fix(launcher): side" >/dev/null
tag v0.2.0
published v0.2.0 "${d29a}"
git -C "${repo}" checkout -q main
run_plan schedule
expect_refusal "schedule: a latest release off HEAD's history is refused" "not in HEAD's history"

# --- the legacy base spelling --------------------------------------------------------------------

# v0.3.0 and earlier write FROM docker:<major>-dind-rootless: a base rebuild or a recovery of them
# resolves the canonical reference, and its build context names the FROM as that commit writes it.
new_repo
dockerfile 29 legacy
release_sha="$(commit "fix(image): legacy spelling")"
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29b}"
run_plan schedule
expect_plan "legacy: a base rebuild of a docker:29 commit" mode=base sha="${release_sha}" version=v0.1.1 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless \
	base_from=docker:29-dind-rootless base_repo=docker base_digest="${d29b}" full_suite=true
run_plan workflow_dispatch 0.1.0
expect_plan "legacy: a recovery of a docker:29 commit" mode=recover sha="${release_sha}" version=v0.1.0 \
	major=29 base_ref=docker.io/library/docker:29-dind-rootless base_from=docker:29-dind-rootless \
	base_repo=docker base_digest="${d29a}" full_suite=true
dockerfile 29
head="$(commit "feat(image): canonical spelling")"
ci "${head}" green
echo v0.2.0 >"${STUB_DIR}/svu_next"
run_plan schedule
expect_plan "legacy: HEAD in the canonical spelling after a legacy release" mode=head sha="${head}" \
	version=v0.2.0 previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless \
	base_digest="${d29b}" full_suite=true

# --- the tag's record: a base rebuild that failed after its tag push -----------------------------

new_repo
release_sha="$(head_sha)"
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29b}"
run_plan schedule
expect_plan "record: the base rebuild" mode=base sha="${release_sha}" version=v0.1.1 previous=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true
# The release job tags it with the record and pushes the tag; the publish fails before any version
# tag. Upstream moves on before the next run.
tag_recorded v0.1.1 "${release_sha}" "${d29b}" "docker.io/library/docker@${d29b}"
upstream_now="sha256:$(printf 'e%.0s' {1..64})"
sed -i "s#^docker.io/library/docker:29-dind-rootless .*#docker.io/library/docker:29-dind-rootless ${upstream_now}#" "${STUB_DIR}/images"
run_plan schedule
expect_plan "record: the next run recovers on the recorded base, with the base diff restored" mode=recover \
	sha="${release_sha}" version=v0.1.1 previous=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true \
	scan_baseline="docker.io/library/docker@${d29b}"
run_plan workflow_dispatch v0.1.1
expect_plan "record: a dispatched recovery restores it too" mode=recover sha="${release_sha}" version=v0.1.1 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" \
	full_suite=true scan_baseline="docker.io/library/docker@${d29b}"
# One variant published on the recorded base: the same plan. On another base: a human.
echo "${image_repo}:v0.1.1-uid568 ${d29b}" >>"${STUB_DIR}/images"
VARIANTS="568:568 1000:1000" run_plan schedule
expect_plan "record: a partly published recovery keeps the recorded base and baseline" mode=recover \
	sha="${release_sha}" version=v0.1.1 previous=v0.1.0 major=29 \
	base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29b}" full_suite=true \
	scan_baseline="docker.io/library/docker@${d29b}"
sed -i "s#^${image_repo}:v0.1.1-uid568 .*#${image_repo}:v0.1.1-uid568 ${d29a}#" "${STUB_DIR}/images"
VARIANTS="568:568 1000:1000" run_plan schedule
expect_refusal "record: published images on another base than the tag records are refused" \
	"are built on ${d29a}, but its tag records ${d29b}"

# A head release's tag records no baseline: its recovery gates on every finding, as it did.
new_repo
tag v0.1.0
published v0.1.0 "${d29a}"
upstream 29 "${d29a}"
head="$(commit "feat(launcher): second")"
tag_recorded v0.2.0 "${head}" "${d29a}" ""
run_plan schedule
expect_plan "record: a head release's recovery has no baseline" mode=recover sha="${head}" version=v0.2.0 \
	previous=v0.1.0 major=29 base_ref=docker.io/library/docker:29-dind-rootless base_digest="${d29a}" \
	full_suite=false

# Annotations that do not parse stop the run.
for bad in "Base: docker.io/library/docker:30-dind-rootless@${d29a}" "Base: docker.io/library/docker:29-dind-rootless" \
	"$(printf 'Base: docker.io/library/docker:29-dind-rootless@%s\nScan baseline: docker.io/library/docker@%s' "${d29a}" "${d29b}")" \
	"Scan baseline: docker.io/library/docker@${d29a}" \
	"$(printf 'Base: docker.io/library/docker:29-dind-rootless@%s\nBase: docker.io/library/docker:29-dind-rootless@%s' "${d29a}" "${d29a}")"; do
	new_repo
	git -C "${repo}" tag -a v0.1.0 -m "$(printf 'swarm-rootless-docker v0.1.0\n\n%s' "${bad}")"
	upstream 29 "${d29a}"
	run_plan schedule
	expect_refusal "record: the annotation '${bad//$'\n'/ | }' is refused" "the tag v0.1.0"
	[[ "${status}" -ne 0 && "${err}" == *"v0.1.0"* ]] || fail "record: '${bad}' failed for another reason: ${err}"
done

# The writer refuses what the reader would.
for args in "docker:29-dind-rootless ${d29a} ''" "docker.io/library/docker:29-dind-rootless sha256:1 ''" \
	"docker.io/library/docker:29-dind-rootless ${d29a} docker.io/library/docker@${d29b}"; do
	status=0
	got="$(eval "env -i PATH=\"\${PATH}\" bash \"\${script}\" record ${args}" 2>&1)" || status=$?
	if [[ "${status}" -ne 0 && "${got}" == *"record:"* ]]; then
		pass "record: the writer refuses ${args}"
	else
		fail "record: the writer took ${args}: exit ${status}, got '${got}'"
	fi
done
got="$(env -i PATH="${PATH}" bash "${script}" record docker.io/library/docker:29-dind-rootless "${d29a}" "")"
if [[ "${got}" == "Base: docker.io/library/docker:29-dind-rootless@${d29a}"$'\n'"Scan baseline: none" ]]; then
	pass "record: the writer's lines"
else
	fail "record: the writer printed '${got}'"
fi

# --- floating ------------------------------------------------------------------------------------

new_repo
tag v0.1.0
dockerfile 30
commit "feat(deps): docker 30" >/dev/null
tag v0.2.0
tag v0.1.1 'v0.1.0^{}'
tag v0.3.0-rc.1
for pair in v0.1.0=false v0.1.1=true v0.2.0=true; do
	status=0
	got="$(cd "${repo}" && env -i PATH="${PATH}" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
		RELEASE_TAG_REGEX='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
		bash "${script}" floating "${pair%%=*}" 2>&1)" || status=$?
	if [[ "${status}" -eq 0 && "${got}" == "${pair#*=}" ]]; then
		pass "floating: ${pair}"
	else
		fail "floating: ${pair}: exit ${status}, got '${got}'"
	fi
done
status=0
got="$(cd "${repo}" && env -i PATH="${PATH}" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
	RELEASE_TAG_REGEX='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
	bash "${script}" floating v0.9.0 2>&1)" || status=$?
if [[ "${status}" -ne 0 && "${got}" == *"v0.9.0 is not tagged"* ]]; then
	pass "floating: an untagged version is refused"
else
	fail "floating: an untagged version: exit ${status}, got '${got}'"
fi

if [[ "${failures}" -ne 0 ]]; then
	echo "${failures} check(s) failed" >&2
	exit 1
fi
echo "every check passed"
