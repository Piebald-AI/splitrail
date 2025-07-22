#!/usr/bin/env bash
# Copied from https://github.com/sharkdp/bat/blob/872d0baafbc575a80618ec847dee6d63dad3e6e3/tests/scripts/license-checks.sh.
set -o errexit -o nounset -o pipefail

# Make sure that we don't accidentally include GPL licenced files
gpl_term="General Public License"
gpl_excludes=(
    # `gpl_term`'s value above matches itself.
    ":(exclude)scripts/license-checks.sh"
)
gpl_occurrences=$(git grep --recurse-submodules "${gpl_term}" -- "${gpl_excludes[@]}" || true)

if [ -z "${gpl_occurrences}" ]; then
    echo "PASS: No files under GPL were found"
else
    echo "FAIL: GPL-licensed files are not allowed, but occurrences of '${gpl_term}' were found:"
    echo "${gpl_occurrences}"
    exit 1
fi
