#!/usr/bin/env bash
#
# License header checker, off until the repository picks a license.
#
# Once enabled, it fails on any tracked code file whose first lines do not carry the project's
# license notice and copyright line, so a missing header is caught as surely as one copied from
# another project. YAML, JSON and Markdown are never governed: they are configuration and
# documentation, not code files in this sense.
#
# The template ships no LICENSE, so nothing runs this yet. To enable it (AGENTS.md, "License
# headers"):
#   1. add the LICENSE and fill in the configuration block below;
#   2. add the header to every file the script lists, this one included, with the year each file
#      was first committed;
#   3. wire the check-license-headers hook into .pre-commit-config.yaml.
#
# Exempt by default: the make-common files the bootstrap fetches (they carry make-common's own MIT
# header; read from the Makefile's MK_COMMON_FILES) and scripts/bootstrap-mk-common.sh, which comes
# from make-common too. Add generated and vendored files to the exemptions, each for a reason.

set -euo pipefail

# --- Configuration -------------------------------------------------------------------------------

# Named in the messages, and in a GPL notice's "This file is part of ..." line.
PROJECT_NAME="template-default"

# The copyright holder, as the copyright line names it.
COPYRIGHT_HOLDER="Roberto Leinardi"

# The license, as the messages name it.
LICENSE_NAME=""

# Lines the header must contain, verbatim. Empty means "not configured", and the check refuses to
# run rather than pass on nothing. Copy one preset, or write your own:
#
#   MIT:
#     "MIT License"
#     "Permission is hereby granted, free of charge, to any person obtaining a copy"
#     "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR"
#   Apache-2.0:
#     "Licensed under the Apache License, Version 2.0 (the \"License\");"
#     "you may not use this file except in compliance with the License."
#   GPL-3.0-or-later:
#     "This file is part of ${PROJECT_NAME}."
#     "it under the terms of the GNU General Public License as published by"
#     "the Free Software Foundation, either version 3 of the License, or"
#     "(at your option) any later version."
REQUIRED_LINES=()

# The copyright line: the year (or years) and the holder, on one line. Matches both
# "Copyright (c) 2026 Name" (MIT, GPL) and "Copyright 2026 Name." (Apache-2.0). An extended regex,
# so a holder with regex metacharacters needs them escaped.
COPYRIGHT_REGEX="Copyright (\\(c\\) )?[0-9]{4}([-, ]+[0-9]{4})* ${COPYRIGHT_HOLDER}\\.?\$"

# How many leading lines the header must sit in: a shebang and a comment line may precede it.
HEADER_WINDOW=30

# Governed file extensions (without the dot), and governed file names matched on the base name.
# Repo-local .mk fragments are governed too; the make-common ones are exempt below.
GOVERNED_EXTENSIONS=(sh bash py go ts tsx mts cts js mjs cjs vue rs rb tf lua)
GOVERNED_NAMES=(Makefile Dockerfile "*.Dockerfile")

# Path prefixes and exact paths that are not this project's own code.
EXEMPT_PREFIXES=()
EXEMPT_PATHS=(
	"scripts/bootstrap-mk-common.sh"
)

# --- End of configuration ------------------------------------------------------------------------

usage() {
	cat <<'USAGE'
Usage: scripts/check-license-headers.sh

Checks every tracked code file for the license header configured at the top of this script and
lists the files that lack it. Run from anywhere inside the repository.
USAGE
}

if [[ $# -gt 0 ]]; then
	usage >&2
	exit 2
fi

if [[ ${#REQUIRED_LINES[@]} -eq 0 || -z ${LICENSE_NAME} ]]; then
	echo "The license-header check is not configured: set LICENSE_NAME and REQUIRED_LINES at the" >&2
	echo "top of scripts/check-license-headers.sh (AGENTS.md, \"License headers\")." >&2
	exit 2
fi

cd "$(git rev-parse --show-toplevel)"

# The shared make fragments, read from the Makefile so the list cannot drift from what the
# bootstrap actually fetches.
mk_common_files() {
	sed -n 's/^MK_COMMON_FILES[[:space:]]*:=[[:space:]]*//p' Makefile
}

is_governed() {
	local path="$1" base entry
	base="${path##*/}"

	if [[ ${path} == .mk/*.mk ]]; then
		return 0
	fi

	for entry in "${GOVERNED_EXTENSIONS[@]}"; do
		if [[ ${base} == *."${entry}" ]]; then
			return 0
		fi
	done

	for entry in "${GOVERNED_NAMES[@]}"; do
		# Unquoted on purpose: an entry may be a glob.
		# shellcheck disable=SC2053
		if [[ ${base} == ${entry} ]]; then
			return 0
		fi
	done

	return 1
}

is_exempt() {
	local path="$1" entry

	for entry in ${EXEMPT_PREFIXES[@]+"${EXEMPT_PREFIXES[@]}"}; do
		if [[ ${path} == "${entry}"* ]]; then
			return 0
		fi
	done

	for entry in ${EXEMPT_PATHS[@]+"${EXEMPT_PATHS[@]}"}; do
		if [[ ${path} == "${entry}" ]]; then
			return 0
		fi
	done

	for entry in ${COMMON_FILES}; do
		if [[ ${path} == ".mk/${entry}" ]]; then
			return 0
		fi
	done

	return 1
}

has_header() {
	local path="$1" head line
	head="$(head -n "${HEADER_WINDOW}" -- "${path}")"

	for line in "${REQUIRED_LINES[@]}"; do
		if [[ ${head} != *"${line}"* ]]; then
			return 1
		fi
	done

	grep -Eq -- "${COPYRIGHT_REGEX}" <<<"${head}"
}

COMMON_FILES="$(mk_common_files)"
if [[ -z ${COMMON_FILES} ]]; then
	echo "Could not read MK_COMMON_FILES from the Makefile." >&2
	exit 1
fi

missing=()
checked=0

while IFS= read -r -d '' path; do
	if ! is_governed "${path}" || is_exempt "${path}"; then
		continue
	fi

	# A tracked file deleted in the working tree is not this check's concern.
	if [[ ! -f ${path} ]]; then
		continue
	fi

	checked=$((checked + 1))

	if ! has_header "${path}"; then
		missing+=("${path}")
	fi
done < <(git ls-files -z)

if [[ ${checked} -eq 0 ]]; then
	echo "No governed files found: the check has nothing to check." >&2
	exit 1
fi

if [[ ${#missing[@]} -gt 0 ]]; then
	echo "These files lack the ${PROJECT_NAME} ${LICENSE_NAME} license header:" >&2
	printf '  %s\n' "${missing[@]}" >&2
	echo "Copy the header from a file that has it, with the year the file was first committed." >&2
	exit 1
fi

echo "License headers: ${checked} files checked, all carry the ${LICENSE_NAME} header."
