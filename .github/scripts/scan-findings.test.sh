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

# Tests for scan-findings.sh. Plain bash: a stub trivy writes, for each reference it is asked to
# scan, the report saved under that reference's name in the scratch directory, and fails for a
# reference with no report.
#
# Run: bash .github/scripts/scan-findings.test.sh

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="${here}/scan-findings.sh"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

cat >"${work}/trivy" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == image ]] || { echo "stub trivy: unexpected call $*" >&2; exit 99; }
shift
output="" platform="" reference=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--output) output="$2"; shift 2 ;;
	--platform) platform="$2"; shift 2 ;;
	--ignorefile | --format | --scanners | --severity) shift 2 ;;
	--ignore-unfixed) shift ;;
	*) reference="$1"; shift ;;
	esac
done
[[ "${platform}" == linux/amd64 ]] || { echo "stub trivy: platform '${platform}'" >&2; exit 99; }
report="${STUB_DIR}/$(tr '/:@' '___' <<<"${reference}").json"
[[ -f "${report}" ]] || { echo "stub trivy: cannot scan ${reference}" >&2; exit 1; }
cp "${report}" "${output}"
STUB
chmod +x "${work}/trivy"

failures=0

# report REFERENCE TARGET FINDING...: the report trivy gives for REFERENCE, whose OS packages it
# names TARGET; each FINDING is "class|target|package|version|id|severity" (an os-pkgs finding's
# target is the distribution).
report() {
	local reference="$1" target="$2" finding
	shift 2
	{
		echo '{"Results": ['
		local first=true
		for finding in "$@"; do
			IFS='|' read -r class place package version id severity <<<"${finding}"
			if [[ "${class}" == os-pkgs ]]; then
				result_target="${target}" type="${place}"
			else
				result_target="${place}" type=gobinary
			fi
			${first} || echo ','
			first=false
			printf '{"Target":"%s","Class":"%s","Type":"%s","Vulnerabilities":[{"PkgName":"%s","InstalledVersion":"%s","VulnerabilityID":"%s","Severity":"%s"}]}' \
				"${result_target}" "${class}" "${type}" "${package}" "${version}" "${id}" "${severity}"
		done
		echo '], "SchemaVersion": 2}'
	} >"${work}/$(tr '/:@' '___' <<<"${reference}").json"
}

# run_scan: scans "image" against "base"; leaves status and out.
run_scan() {
	status=0
	out="$(cd "${work}" && STUB_DIR="${work}" TRIVY="${work}/trivy" PLATFORMS="${PLATFORMS-linux/amd64}" \
		GITHUB_STEP_SUMMARY="${work}/summary" bash "${script}" image base 2>&1)" || status=$?
}

expect() {
	local name="$1" want_status="$2" want_text="$3"
	if [[ "${status}" -ne "${want_status}" ]]; then
		echo "FAIL ${name}: exit ${status}, want ${want_status}: ${out}" >&2
		failures=$((failures + 1))
	elif ! grep -qF -- "${want_text}" <<<"${out}"; then
		echo "FAIL ${name}: want '${want_text}' in: ${out}" >&2
		failures=$((failures + 1))
	else
		echo "ok   ${name}"
	fi
}

curl_finding="os-pkgs|alpine|libcurl|8.22.0-r0|CVE-1|HIGH"
runc_finding="lang-pkgs|usr/local/bin/runc|golang.org/x/net|v0.55.0|CVE-2|HIGH"

report base "docker:29-dind-rootless (alpine 3.24.3)" "${curl_finding}" "${runc_finding}"
report image "localhost:5000/example@sha256:1 (alpine 3.24.3)" "${curl_finding}" "${runc_finding}"
run_scan
expect "the base's own findings are inherited, not failed" 0 "inherits 2 fixable HIGH or CRITICAL findings"
if grep -qF "golang.org/x/net v0.55.0 CVE-2" "${work}/summary"; then
	echo "ok   the inherited findings are in the summary"
else
	echo "FAIL the inherited findings are not in the summary" >&2
	failures=$((failures + 1))
fi

report image "localhost:5000/example@sha256:1 (alpine 3.24.3)" "${curl_finding}" "${runc_finding}" \
	"lang-pkgs|usr/local/bin/swarm-rootless-docker|tokio|1.0.0|CVE-3|CRITICAL"
run_scan
expect "a finding the base does not carry fails" 1 "lang-pkgs usr/local/bin/swarm-rootless-docker tokio 1.0.0 CVE-3 CRITICAL"

report image "localhost:5000/example@sha256:1 (alpine 3.24.3)" "os-pkgs|alpine|libcurl|8.21.0-r0|CVE-1|HIGH"
run_scan
expect "the same vulnerability in another package version fails" 1 "os-pkgs alpine libcurl 8.21.0-r0 CVE-1 HIGH"

report image "localhost:5000/example@sha256:1 (alpine 3.24.3)" "lang-pkgs|usr/local/bin/ctr|golang.org/x/net|v0.55.0|CVE-2|HIGH"
run_scan
expect "the same vulnerability in another binary fails" 1 "lang-pkgs usr/local/bin/ctr golang.org/x/net v0.55.0 CVE-2 HIGH"

report image "localhost:5000/example@sha256:1 (alpine 3.24.3)"
run_scan
expect "an image with no finding passes" 0 "adds no fixable HIGH or CRITICAL finding"

rm "${work}/base.json"
run_scan
expect "a base that cannot be scanned fails" 1 "cannot scan base"

report base "docker:29-dind-rootless (alpine 3.24.3)"
PLATFORMS="linux/amd64,linux/arm64" run_scan
expect "two platforms are refused" 2 "exactly one platform"

if [[ "${failures}" -ne 0 ]]; then
	echo "${failures} check(s) failed" >&2
	exit 1
fi
echo "every check passed"
