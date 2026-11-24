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

# Tests for trivyignore-expiry.sh, against scratch ignore files, with TODAY fixed.
#
# Run: bash .github/scripts/trivyignore-expiry.test.sh

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="${here}/trivyignore-expiry.sh"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

failures=0

# expect <name> <status> <output pattern> <days> <yaml>
expect() {
	local name="$1" want="$2" pattern="$3" days="$4" yaml="$5" got=0 out
	printf '%s\n' "${yaml}" >"${work}/ignore.yaml"
	out="$(TODAY=2026-11-24 bash "${script}" "${work}/ignore.yaml" "${days}" 2>&1)" || got=$?
	if [[ "${got}" != "${want}" ]] || ! grep -qE -- "${pattern}" <<<"${out}"; then
		echo "FAIL ${name}: exit ${got} (want ${want}), output:" >&2
		printf '    %s\n' "${out//$'\n'/$'\n'    }" >&2
		failures=$((failures + 1))
	else
		echo "ok   ${name}"
	fi
}

entry() { printf '  - id: %s\n    statement: s\n    expired_at: %s\n' "$1" "$2"; }

expect "far expiry passes" 0 "1 entries, none expiring" 10 \
	"vulnerabilities:
$(entry CVE-1 2026-12-05)"
expect "expiry within the window fails" 1 "CVE-1 expires on 2026-12-04, in 10 days" 10 \
	"vulnerabilities:
$(entry CVE-1 2026-12-04)"
expect "expiry today fails" 1 "CVE-1 expired on 2026-11-24" 10 \
	"vulnerabilities:
$(entry CVE-1 2026-11-24)"
expect "past expiry fails" 1 "CVE-1 expired on 2026-11-23" 0 \
	"vulnerabilities:
$(entry CVE-1 2026-11-23)"
expect "one soon entry among several fails" 1 "CVE-2 expires" 10 \
	"vulnerabilities:
$(entry CVE-1 2027-01-01)
$(entry CVE-2 2026-11-30)"
expect "other sections are checked" 1 "AVD-1 expires" 10 \
	"misconfigurations:
$(entry AVD-1 2026-11-30)"
expect "a missing expired_at fails" 1 "CVE-1: expired_at '' is not" 10 \
	"vulnerabilities:
  - id: CVE-1
    statement: s"
expect "an impossible date fails" 1 "expired_at '2026-02-30' is not" 10 \
	"vulnerabilities:
$(entry CVE-1 2026-02-30)"
expect "a timestamp fails" 1 "is not a YYYY-MM-DD date" 10 \
	"vulnerabilities:
$(entry CVE-1 2027-01-01T00:00:00Z)"
expect "anchors and aliases resolve" 0 "2 entries" 10 \
	"vulnerabilities:
  - id: CVE-1
    statement: &s text
    expired_at: 2027-01-01
  - id: CVE-2
    statement: *s
    expired_at: 2027-01-01"
expect "an empty file passes" 0 "0 entries" 10 "vulnerabilities: []"
expect "a bad DAYS refuses" 2 "is not a whole number" x "vulnerabilities: []"

if ((failures)); then
	echo "${failures} test(s) failed." >&2
	exit 1
fi
echo "All trivyignore-expiry.sh tests passed."
