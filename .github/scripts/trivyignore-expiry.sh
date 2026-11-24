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
#
# Refuses a Trivy ignore file whose entries expire soon. Trivy enforces each entry's expired_at
# itself: on that date the finding fails every scan again, the daily release's included. This
# check fails first, from the weekly scheduled scan, while the entries can still be reviewed
# (dropped when the current base digest no longer needs them, extended otherwise) without a
# release failing on them.
#
#   trivyignore-expiry.sh [FILE] [DAYS]
#       FILE defaults to .trivyignore.yaml and DAYS to 10, more than the scheduled scan's week, so
#       an entry is reported at least once before it expires. Fails on an entry that expires
#       within DAYS days or already has, and, failing closed, on one without an expired_at or
#       with one that is not a YYYY-MM-DD date. TODAY (YYYY-MM-DD) overrides the date, for tests.
#
# Needs GNU date and mikefarah's yq (both on GitHub's ubuntu runners).

set -euo pipefail

file="${1:-.trivyignore.yaml}"
days="${2:-10}"

if ! [[ "${days}" =~ ^[0-9]+$ ]]; then
	echo "::error::trivyignore-expiry: DAYS '${days}' is not a whole number." >&2
	exit 2
fi
if [[ ! -f "${file}" ]]; then
	echo "::error::trivyignore-expiry: ${file} does not exist." >&2
	exit 2
fi

# A date as days since the epoch, or nothing when it is not a real YYYY-MM-DD date.
to_day() {
	local value="$1" seconds
	[[ "${value}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || return 0
	seconds="$(date -u -d "${value}" +%s 2>/dev/null)" || return 0
	# date accepts some dates it then normalises; only one that prints back unchanged is real.
	[[ "$(date -u -d "@${seconds}" +%F)" == "${value}" ]] || return 0
	echo $((seconds / 86400))
}

today="${TODAY:-$(date -u +%F)}"
today_day="$(to_day "${today}")"
if [[ -z "${today_day}" ]]; then
	echo "::error::trivyignore-expiry: TODAY '${today}' is not a YYYY-MM-DD date." >&2
	exit 2
fi

# Every entry of every section (vulnerabilities, misconfigurations, secrets, licenses).
entries="$(yq -r '.[] | .[] | [(.id // ""), (.expired_at // "" | tostring)] | @tsv' - <"${file}")"

failed=0
count=0
while IFS=$'\t' read -r id expiry; do
	[[ -n "${id}${expiry}" ]] || continue
	count=$((count + 1))
	expiry_day="$(to_day "${expiry}")"
	if [[ -z "${expiry_day}" ]]; then
		echo "::error file=${file}::${id:-an entry without an id}: expired_at '${expiry}' is not a YYYY-MM-DD date."
		failed=1
		continue
	fi
	left=$((expiry_day - today_day))
	if ((left <= 0)); then
		echo "::error file=${file}::${id} expired on ${expiry}: Trivy counts the finding again."
		failed=1
	elif ((left <= days)); then
		echo "::error file=${file}::${id} expires on ${expiry}, in ${left} days: review it before then."
		failed=1
	fi
done <<<"${entries}"

if ((failed)); then
	echo "Drop each entry the current base digest no longer needs, extend the rest after a scan with" >&2
	echo "the latest Trivy (docs/release.md, Scheduled scan)." >&2
	exit 1
fi
echo "${file}: ${count} entries, none expiring within ${days} days of ${today}."
