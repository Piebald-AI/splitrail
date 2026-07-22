# Rewrite-retention regression check (Claude Code)

End-to-end guard for the #204 fix (issue #200): Claude Code rewrites session
JSONL files in place on resume/compact, and splitrail's totals must not drift
when that happens, because the local history store restores retained records.

Everything runs against a synthetic corpus under an isolated `$HOME` — no real
`~/.claude` data is read, no config is loaded, no upload path exists.

## Run

```bash
cargo build --release
scripts/rewrite-retention-check/run_check.sh
```

Optionally also demonstrate the pre-fix drift with an old binary:

```bash
SPLITRAIL_BASELINE_BIN=/path/to/splitrail-3.5.9 \
  scripts/rewrite-retention-check/run_check.sh
```

## What it asserts

| # | assertion | guards |
|---|---|---|
| R1 | scan of pristine corpus == generator manifest (per-model message counts + 4 token fields) | parser + streaming-line dedup (`local_hash` + token fingerprint) |
| R2 | scan after a simulated resume/compact == scan before it | **the history-store retention itself — the #200/#204 regression** |
| R3 | three consecutive scans byte-identical | restart stability of the store |
| R4 | (only with `SPLITRAIL_BASELINE_BIN`) pre-fix binary drops by exactly the removed usage | demonstrates the bug the fixture pins down |

The synthetic corpus mirrors the parts of Claude Code's format the check
depends on: each API message written as two streaming lines (distinct line
`uuid`s, same `message.id`/`requestId`, identical usage), plus skip-cases
(user lines, `summary`, `file-history-snapshot`, `<synthetic>` api-error).
Costs are asserted only *within* a binary (R2/R3), never against the
manifest, so pricing-table changes can't break the check.

## CI

```yaml
- run: cargo build --release
- run: scripts/rewrite-retention-check/run_check.sh
```

Unix (macOS/Linux) with python3 ≥ 3.9 on PATH; Windows untested (WSL works).

## Provenance

Distilled from the validation run posted in #200: all four assertions passed
against a ~50-day real corpus (3.5.9 vs 3.6.0, frozen-snapshot A/B), with
3.6.0's main-transcript totals token-exact against an independent
append-only ingest log. The check fails on 3.5.9 (R2) and passes on 3.6.0.
