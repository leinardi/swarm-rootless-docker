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
# Decides what release.yaml releases: which commit, as which version, on which base. Run from a
# clone with the full history and every tag, on the commit the workflow runs from (main).
#
#   release-plan.sh plan
#       Appends mode, sha, version, previous, major, base_ref and base_digest to $GITHUB_OUTPUT.
#       The modes:
#         head     a new version from HEAD: releasable commits since the last tag (or an explicit
#                  version), and ci-result green on HEAD.
#         base     the latest release's own commit again, as the next patch version, because the
#                  upstream base it was built on has a new digest (schedule only).
#         recover  a version whose tag exists, again from its own commit and on the base its
#                  published images record: a publish that failed part-way is finished, and
#                  nothing already published is replaced.
#         none     nothing to do (schedule only).
#   release-plan.sh floating VERSION
#       Prints true when VERSION is the highest release of the Docker major its own Dockerfile
#       names, so the floating <major>-uid<UID> tags may point at it, and false otherwise.
#
# Environment: EVENT (schedule or workflow_dispatch), VERSION_INPUT (the dispatch's version, may be
# empty), REPOSITORY (owner/name), IMAGE_REPO, IMAGE_VARIANTS (UID:GID pairs, space-separated),
# RELEASE_TAG_REGEX, GITHUB_OUTPUT, and optionally GITHUB_STEP_SUMMARY and SVU (the svu command).
#
# Every lookup fails closed: only a definite "not found" counts as absent, and any other error
# stops the run, so an outage never reads as "nothing to release" or as "not released yet".

set -euo pipefail

DOCKERFILE=deployments/docker/Dockerfile
SVU="${SVU:-svu}"

die() {
	echo "release-plan: $*" >&2
	exit 1
}

note() {
	echo "release-plan: $*" >&2
}

warn() {
	echo "::warning::$*"
	if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
		echo "- $*" >>"${GITHUB_STEP_SUMMARY}"
	fi
}

require_env() {
	local name
	for name in "$@"; do
		[[ -n "${!name:-}" ]] || die "${name} is not set"
	done
}

# The release tags in the input, by the one definition of a release tag.
release_only() {
	{ grep -E "${RELEASE_TAG_REGEX}" || true; }
}

highest() {
	sort -V | tail -n1
}

# Whether $1 is a higher version than $2 (an empty $2 is lower than everything).
higher() {
	[[ "$1" != "$2" ]] && [[ "$(printf '%s\n%s\n' "$1" "$2" | highest)" == "$1" ]]
}

tag_exists() {
	git rev-parse -q --verify "refs/tags/$1" >/dev/null
}

commit_of() {
	git rev-parse "$1^{commit}"
}

next_patch() {
	local core="${1#v}"
	local major="${core%%.*}" rest="${core#*.}"
	local minor="${rest%%.*}" patch="${rest#*.}"
	echo "v${major}.${minor}.$((patch + 1))"
}

# The Docker major of the runtime stage's FROM in commit $1's Dockerfile: exactly one stage named
# runtime, FROM docker:<major>-dind-rootless with no digest and no flags.
docker_major() {
	local content lines image
	local -a fields
	content="$(git show "$1:${DOCKERFILE}")" || die "commit $1 has no ${DOCKERFILE}"
	lines="$(awk 'toupper($1) == "FROM" && NF >= 4 && toupper($(NF - 1)) == "AS" && $NF == "runtime"' <<<"${content}")"
	[[ -n "${lines}" && "$(wc -l <<<"${lines}")" -eq 1 ]] ||
		die "commit $1's ${DOCKERFILE} does not have exactly one stage named runtime"
	read -r -a fields <<<"${lines}"
	[[ "${#fields[@]}" -eq 4 ]] || die "commit $1's runtime stage is '${lines}', want FROM <image> AS runtime"
	image="${fields[1]}"
	[[ "${image}" =~ ^docker:([1-9][0-9]*)-dind-rootless$ ]] ||
		die "commit $1's runtime stage is FROM ${image}, want docker:<major>-dind-rootless"
	echo "${BASH_REMATCH[1]}"
}

# The UIDs of IMAGE_VARIANTS, one per line, after checking every pair. The UID names the tag
# (-uid<UID>), so two variants may not share one.
variant_uids() {
	local variant uid gid seen=" "
	for variant in ${IMAGE_VARIANTS}; do
		[[ "${variant}" =~ ^([1-9][0-9]*):([1-9][0-9]*)$ ]] || die "IMAGE_VARIANTS entry '${variant}' is not UID:GID"
		uid="${BASH_REMATCH[1]}"
		gid="${BASH_REMATCH[2]}"
		[[ "${uid}" -lt 4294967295 && "${gid}" -lt 4294967295 ]] || die "IMAGE_VARIANTS entry '${variant}' is out of range"
		[[ "${seen}" != *" ${uid} "* ]] || die "IMAGE_VARIANTS names UID ${uid} twice"
		seen+="${uid} "
		echo "${uid}"
	done
	[[ "${seen}" != " " ]] || die "IMAGE_VARIANTS is empty"
}

# ci-result for commit $1: green, red, pending or missing. Only check runs of GitHub Actions count,
# so no other app can pass the check by naming a run ci-result, and every latest run of it (a push
# and a dispatch run each leave one) must have succeeded.
ci_state() {
	local response
	response="$(gh api "repos/${REPOSITORY}/commits/$1/check-runs?check_name=ci-result&filter=latest&per_page=100")" ||
		die "could not read ci-result for $1"
	jq -r '
		[.check_runs[] | select(.app.slug == "github-actions")] as $runs
		| if ($runs | length) == 0 then "missing"
		  elif any($runs[]; .status != "completed") then "pending"
		  elif all($runs[]; .conclusion == "success") then "green"
		  else "red" end
	' <<<"${response}" || die "could not parse ci-result for $1"
}

require_green() {
	local state
	state="$(ci_state "$1")"
	[[ "${state}" == green ]] || die "ci-result is ${state} on $1 ($2); a release needs it green on that exact commit"
}

# imagetools inspect of $1 with the format $2. Prints the JSON; returns 1 for a reference that
# does not exist, and stops the run on any other error.
inspect() {
	local out
	if out="$(docker buildx imagetools inspect "$1" --format "$2" 2>&1)"; then
		echo "${out}"
		return 0
	fi
	if grep -qiE 'not found|manifest unknown' <<<"${out}"; then
		return 1
	fi
	die "could not inspect $1: ${out}"
}

# The index digest upstream serves for docker:<major>-dind-rootless now.
upstream_digest() {
	local json digest
	json="$(inspect "docker.io/library/$1" '{{json .Manifest}}')" || die "upstream has no $1"
	digest="$(jq -r '.digest' <<<"${json}")"
	[[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]] || die "upstream $1 has no index digest (got '${digest}')"
	echo "${digest}"
}

# The base digest label of the published image $1, which must exist.
published_base() {
	local json
	json="$(inspect "$1" '{{json .Image}}')" || die "$1 is not published"
	# One platform: the image's config. Several: a map of them, all built on the same base.
	jq -r '
		(if has("config") then [.] else [.[]] end)
		| map(.config.Labels["org.opencontainers.image.base.digest"] // "")
		| unique | if length == 1 then .[0] else "mixed" end
	' <<<"${json}" || die "could not read the labels of $1"
}

# Why release $1 is unfinished, or nothing when every variant's version tag is published and its
# GitHub release is out of draft.
unfinished() {
	local uid out
	for uid in $(variant_uids); do
		if ! inspect "${IMAGE_REPO}:$1-uid${uid}" '{{json .Manifest}}' >/dev/null; then
			echo "${IMAGE_REPO}:$1-uid${uid} is not published"
			return
		fi
	done
	if ! out="$(gh release view "$1" --repo "${REPOSITORY}" --json isDraft --jq '.isDraft' 2>&1)"; then
		grep -qi 'release not found' <<<"${out}" || die "could not read the release $1: ${out}"
		echo "the GitHub release $1 does not exist"
	elif [[ "${out}" == true ]]; then
		echo "the GitHub release $1 is still a draft"
	elif [[ "${out}" != false ]]; then
		die "the release $1 reports isDraft '${out}'"
	fi
}

emit() {
	local mode="$1" sha="${2:-}" version="${3:-}" previous="${4:-}" major="" base_ref="" base_digest=""
	if [[ "${mode}" != none ]]; then
		major="$(docker_major "${sha}")"
		base_ref="docker:${major}-dind-rootless"
		base_digest="${5:-}"
		if [[ -z "${base_digest}" ]]; then
			base_digest="$(upstream_digest "${base_ref}")"
		fi
		note "${mode}: ${version} from ${sha} on ${base_ref}@${base_digest} (previous: ${previous:-none})"
	fi
	{
		echo "mode=${mode}"
		echo "sha=${sha}"
		echo "version=${version}"
		echo "previous=${previous}"
		echo "major=${major}"
		echo "base_ref=${base_ref}"
		echo "base_digest=${base_digest}"
	} >>"${GITHUB_OUTPUT}"
	exit 0
}

# The highest release below $1, or nothing.
below() {
	local tag
	for tag in $(git tag --list | release_only); do
		if higher "$1" "${tag}"; then
			echo "${tag}"
		fi
	done | highest
}

# The base digest the published variants of release $1 record, or nothing when none is published.
# Variants on different bases, or a recorded base that is not a digest, stop the run: a recovery
# could not then build the rest on the same base, nor say which one the release is on.
recorded_base() {
	local uid bases=""
	for uid in $(variant_uids); do
		if inspect "${IMAGE_REPO}:$1-uid${uid}" '{{json .Manifest}}' >/dev/null; then
			bases+="$(published_base "${IMAGE_REPO}:$1-uid${uid}")"$'\n'
		fi
	done
	bases="$(sort -u <<<"${bases}" | sed '/^$/d')"
	if [[ -z "${bases}" ]]; then
		return
	fi
	# One line only: the anchors hold for the whole value, not per line.
	[[ "${bases}" =~ ^sha256:[0-9a-f]{64}$ ]] ||
		die "the published images of $1 record the bases '${bases//$'\n'/ }', not one digest"
	echo "${bases}"
}

# A version whose tag already exists, from the tag's own commit, on the base its published images
# record: the variants still to build match them, and the release names the base they are on.
recover() {
	local sha previous base
	sha="$(commit_of "$1")"
	require_green "${sha}" "the commit $1 is tagged on"
	previous="$(below "$1")"
	base="$(recorded_base "$1")"
	emit recover "${sha}" "$1" "${previous}" "${base}"
}

plan() {
	require_env EVENT REPOSITORY IMAGE_REPO IMAGE_VARIANTS RELEASE_TAG_REGEX GITHUB_OUTPUT
	variant_uids >/dev/null
	case "${EVENT}" in
	schedule | workflow_dispatch) ;;
	*) die "EVENT must be schedule or workflow_dispatch, got '${EVENT}'" ;;
	esac

	local head latest current next version reason state
	head="$(git rev-parse HEAD)"
	latest="$(git tag --list | release_only | highest)"

	if [[ -n "${VERSION_INPUT:-}" ]]; then
		[[ "${EVENT}" == workflow_dispatch ]] || die "only a dispatch takes a version"
		version="v${VERSION_INPUT#v}"
		[[ "${version}" =~ ${RELEASE_TAG_REGEX} ]] ||
			die "the version must be X.Y.Z or vX.Y.Z with no leading zeros, no pre-release and no build metadata (got '${VERSION_INPUT}')"
		if tag_exists "${version}"; then
			recover "${version}"
		fi
		higher "${version}" "${latest}" ||
			die "${version} is not higher than the latest release ${latest}; tags are immutable, so only a higher version may be published"
		require_green "${head}" HEAD
		emit head "${head}" "${version}" "${latest}"
	fi

	# A release that failed part-way is finished before anything new is started.
	if [[ -n "${latest}" ]]; then
		reason="$(unfinished "${latest}")"
		if [[ -n "${reason}" ]]; then
			note "finishing ${latest}: ${reason}"
			recover "${latest}"
		fi
	fi

	current="$(git tag --merged HEAD | release_only | highest)"
	if [[ "${current}" != "${latest}" ]]; then
		die "the latest release ${latest} is tagged on a commit that is not in HEAD's history (HEAD's latest is ${current:-none})"
	fi
	if [[ -z "${current}" ]]; then
		[[ "${EVENT}" == schedule ]] || die "no vX.Y.Z tag is reachable from HEAD: the first release needs an explicit version"
		warn "Nothing released: there is no release yet, and the first one is dispatched with an explicit version."
		emit none
	fi

	# svu is given the exact tag to count from, never a glob: a glob would let a stray tag such as
	# v1.9.0-rc.1 become the current version. A strict tag, as a glob, matches only itself.
	next="$("${SVU}" next --tag.pattern "${current}")" || die "svu failed"
	[[ "${next}" =~ ${RELEASE_TAG_REGEX} ]] || die "svu derived '${next}' from ${current}, which is not a vX.Y.Z tag"
	if [[ "${next}" != "${current}" ]]; then
		higher "${next}" "${current}" || die "svu derived ${next}, which is not higher than ${current}"
		state="$(ci_state "${head}")"
		if [[ "${state}" == green ]]; then
			emit head "${head}" "${next}" "${current}"
		fi
		[[ "${EVENT}" == schedule ]] || die "ci-result is ${state} on HEAD (${head}); a release needs it green on that exact commit"
		warn "Commits since ${current} would release ${next}, but ci-result is ${state} on HEAD (${head}). They wait for a green run; only a base rebuild of ${current} was considered."
	elif [[ "${EVENT}" == workflow_dispatch ]]; then
		die "no feat/fix or breaking-change commit since ${current}, so there is nothing to bump; dispatch an explicit version to force one"
	fi

	# The base rebuild: the latest release's own commit, its own Dockerfile's Docker major, and
	# only when upstream now serves another digest than the one every variant was built on.
	local sha major base_ref upstream uid built
	sha="$(commit_of "${latest}")"
	major="$(docker_major "${sha}")"
	base_ref="docker:${major}-dind-rootless"
	upstream="$(upstream_digest "${base_ref}")"
	for uid in $(variant_uids); do
		built="$(published_base "${IMAGE_REPO}:${latest}-uid${uid}")"
		if [[ "${built}" != "${upstream}" ]]; then
			note "${IMAGE_REPO}:${latest}-uid${uid} is built on ${built:-an unrecorded base}; upstream ${base_ref} is ${upstream}"
			require_green "${sha}" "the commit ${latest} is tagged on"
			emit base "${sha}" "$(next_patch "${latest}")" "${latest}" "${upstream}"
		fi
	done
	note "nothing to release: ${latest} is built on upstream's current ${base_ref} (${upstream})"
	emit none
}

floating() {
	require_env RELEASE_TAG_REGEX
	local version="$1" major tag sha tag_major highest_of_major=""
	[[ "${version}" =~ ${RELEASE_TAG_REGEX} ]] || die "'${version}' is not a vX.Y.Z tag"
	tag_exists "${version}" || die "${version} is not tagged"
	sha="$(commit_of "${version}")"
	major="$(docker_major "${sha}")"
	for tag in $(git tag --list | release_only); do
		sha="$(commit_of "${tag}")"
		tag_major="$(docker_major "${sha}")"
		if [[ "${tag_major}" == "${major}" ]]; then
			highest_of_major="$(printf '%s\n%s\n' "${highest_of_major}" "${tag}" | highest)"
		fi
	done
	if [[ "${highest_of_major}" == "${version}" ]]; then
		echo true
	else
		echo false
	fi
}

case "${1:-}" in
plan)
	[[ $# -eq 1 ]] || die "usage: $0 plan"
	plan
	;;
floating)
	[[ $# -eq 2 ]] || die "usage: $0 floating VERSION"
	floating "$2"
	;;
*)
	die "usage: $0 plan | floating VERSION"
	;;
esac
