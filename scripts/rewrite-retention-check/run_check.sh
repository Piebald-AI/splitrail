#!/usr/bin/env bash
# End-to-end regression check: Claude Code usage must survive session rewrites.
#
# Guards the #204 fix (issue #200): resume/compact rewrites session JSONL
# files in place; totals must not drift because the history store restores
# retained records.
#
# Runs entirely against a synthetic corpus under an isolated $HOME:
# no real ~/.claude data touched, no config, no upload path.
#
# Usage:
#   scripts/rewrite-retention-check/run_check.sh
#
# Env:
#   SPLITRAIL_BIN           binary under test (default: target/release/splitrail)
#   SPLITRAIL_BASELINE_BIN  optional pre-fix binary (e.g. 3.5.9) to also
#                           demonstrate the drift it exhibited (assertion R4)
#   WORK                    scratch dir (default: mktemp; kept on failure)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BIN="${SPLITRAIL_BIN:-$REPO_ROOT/target/release/splitrail}"
BASE_BIN="${SPLITRAIL_BASELINE_BIN:-}"
WORK="${WORK:-$(mktemp -d "${TMPDIR:-/tmp}/splitrail-rrc.XXXXXX")}"
FAKEHOME="$WORK/home"

if [ ! -x "$BIN" ]; then
  echo "error: splitrail binary not found at $BIN" >&2
  echo "build it first (cargo build --release) or set SPLITRAIL_BIN" >&2
  exit 2
fi

echo "binary under test : $("$BIN" --version)  [$BIN]"
if [ -n "$BASE_BIN" ]; then
  echo "baseline binary   : $("$BASE_BIN" --version)  [$BASE_BIN]"
fi
echo "work dir          : $WORK"

scan() { # $1=binary $2=outfile
  # HOME plus explicit XDG overrides: on Linux the history store honors
  # XDG_STATE_HOME/XDG_DATA_HOME, which would otherwise escape the isolation.
  HOME="$FAKEHOME" \
  XDG_STATE_HOME="$FAKEHOME/.local/state" \
  XDG_DATA_HOME="$FAKEHOME/.local/share" \
    "$1" stats --pretty > "$2" 2> "$2.err" \
    || { echo "scan failed; see $2.err" >&2; exit 1; }
}

python3 "$SCRIPT_DIR/gen_corpus.py" "$FAKEHOME/.claude/projects" \
  --manifest "$WORK/corpus-manifest.json"

# Baseline scans of the pristine corpus. The bin-under-test scan also
# populates its history store (under the isolated $HOME) pre-rewrite.
scan "$BIN" "$WORK/base.json"
if [ -n "$BASE_BIN" ]; then
  scan "$BASE_BIN" "$WORK/base-baseline.json"
fi

# Simulated resume/compact: drop the last 3 assistant message-groups.
python3 "$SCRIPT_DIR/simulate_rewrite.py" "$FAKEHOME/.claude/projects" \
  --drop 3 --manifest "$WORK/drift-manifest.json"

# Post-rewrite scans + restart-stability loop.
scan "$BIN" "$WORK/post.json"
for i in 1 2 3; do scan "$BIN" "$WORK/stab-$i.json"; done
if [ -n "$BASE_BIN" ]; then
  scan "$BASE_BIN" "$WORK/post-baseline.json"
fi

if python3 "$SCRIPT_DIR/check_retention.py" --work "$WORK" \
     ${BASE_BIN:+--baseline}; then
  rm -rf "$WORK"
else
  echo "scratch kept for inspection: $WORK" >&2
  exit 1
fi
