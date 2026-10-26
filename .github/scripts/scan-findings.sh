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
# Scans an image and the base it was built on, and fails only on what the image adds:
#
#   scan-findings.sh IMAGE BASE
#
# Both are scanned for HIGH and CRITICAL vulnerabilities that have a fix upstream (--ignore-unfixed),
# with the exceptions in IGNOREFILE (default .trivyignore.yaml), on the one platform PLATFORMS
# names. A finding in IMAGE that BASE carries too is upstream's, which nothing here can fix: it is
# reported as a warning (and in $GITHUB_STEP_SUMMARY when set), and a rebuild on upstream's next
# digest ships the fix. A finding BASE does not carry fails the scan (exit 1). A finding is the same
# when its package, version, vulnerability, severity and place agree; an OS package is placed by
# its distribution, not by the scan target's name, which differs between the two images.
#
# Environment: PLATFORMS, and optionally IGNOREFILE and TRIVY (the trivy command).

set -euo pipefail

if [[ $# -ne 2 ]]; then
	echo "usage: $0 IMAGE BASE" >&2
	exit 2
fi
image="$1"
base="$2"
ignorefile="${IGNOREFILE:-.trivyignore.yaml}"
trivy="${TRIVY:-trivy}"
[[ -n "${PLATFORMS:-}" && "${PLATFORMS}" != *,* ]] || {
	echo "PLATFORMS must name exactly one platform, got '${PLATFORMS:-}'" >&2
	exit 2
}

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# scan REFERENCE NAME: the findings of REFERENCE, one sorted line each, in $work/NAME.
scan() {
	"${trivy}" image --scanners vuln --severity HIGH,CRITICAL --ignore-unfixed \
		--ignorefile "${ignorefile}" --format json --output "${work}/$2.json" --platform "${PLATFORMS}" "$1"
	jq -r '
		.Results[]? | . as $result | ($result.Vulnerabilities // [])[]
		| [$result.Class, (if $result.Class == "os-pkgs" then $result.Type else $result.Target end),
		   .PkgName, .InstalledVersion, .VulnerabilityID, .Severity] | join(" ")
	' "${work}/$2.json" | sort -u >"${work}/$2"
}

scan "${base}" base
scan "${image}" image
added="$(comm -23 "${work}/image" "${work}/base")"
inherited="$(comm -12 "${work}/image" "${work}/base")"

if [[ -n "${inherited}" ]]; then
	echo "::warning::${image} inherits $(wc -l <<<"${inherited}") fixable HIGH or CRITICAL findings from ${base}; a rebuild on upstream's next digest ships the fixes."
	echo "${inherited}"
	if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
		{
			echo "## Findings ${image} inherits from its base"
			echo
			echo '```text'
			echo "${inherited}"
			echo '```'
		} >>"${GITHUB_STEP_SUMMARY}"
	fi
fi
if [[ -n "${added}" ]]; then
	echo "::error::${image} adds fixable HIGH or CRITICAL findings to ${base}:"
	echo "${added}"
	exit 1
fi
echo "${image} adds no fixable HIGH or CRITICAL finding to ${base}."
