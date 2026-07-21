#!/usr/bin/env python3
"""Assert splitrail's Claude Code rewrite-retention behavior on the synthetic corpus.

Reads scan outputs produced by run_check.sh from --work:

    corpus-manifest.json  drift-manifest.json
    base.json  post.json  stab-1..3.json
    base-baseline.json / post-baseline.json   (only with --baseline)

Assertions
    R1 parse+dedup sanity : base scan == corpus manifest (per-model msgs + 4 token fields)
    R2 rewrite retention  : post scan == base scan (incl. cost — THE regression guard)
    R3 restart stability  : stab-1..3 byte-identical
    R4 drift demo         : baseline binary drops by exactly the removed usage
                            (convention-agnostic: last_line or sum_lines)

Exit 0 iff all applicable assertions pass.
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

TOKEN_KEYS = ("inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens")
MANIFEST_TO_SR = {
    "input_tokens": "inputTokens",
    "output_tokens": "outputTokens",
    "cache_creation_input_tokens": "cacheCreationTokens",
    "cache_read_input_tokens": "cacheReadTokens",
}


def cc_by_model(path: Path) -> dict[str, dict]:
    data = json.loads(path.read_text())
    out: dict[str, dict] = defaultdict(
        lambda: {k: 0 for k in ("messageCount", *TOKEN_KEYS)} | {"cost": 0.0})
    for a in data.get("analyzer_stats", []):
        if a.get("analyzer_name") != "Claude Code":
            continue
        for day in (a.get("daily_stats") or {}).values():
            for model, ms in (day.get("model_stats") or {}).items():
                slot = out[model]
                slot["messageCount"] += int(ms.get("messageCount") or 0)
                for k in TOKEN_KEYS:
                    slot[k] += int(ms.get(k) or 0)
                slot["cost"] += float(ms.get("cost") or 0.0)
    return dict(out)


def int_view(by_model: dict[str, dict]) -> dict[str, dict]:
    return {m: {k: int(v[k]) for k in ("messageCount", *TOKEN_KEYS)}
            for m, v in by_model.items()}


def diff(a: dict[str, dict], b: dict[str, dict]) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for m in sorted(set(a) | set(b)):
        d = {k: a.get(m, {}).get(k, 0) - b.get(m, {}).get(k, 0)
             for k in ("messageCount", *TOKEN_KEYS)}
        if any(d.values()):
            out[m] = d
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--baseline", action="store_true")
    args = ap.parse_args()
    w = args.work

    corpus = json.loads((w / "corpus-manifest.json").read_text())["expected_by_model"]
    base = cc_by_model(w / "base.json")
    post = cc_by_model(w / "post.json")

    results: list[tuple[str, bool, str]] = []

    # R1 — parse + dedup sanity
    expected = {m: {"messageCount": v["messages"],
                    **{MANIFEST_TO_SR[k]: v[k] for k in MANIFEST_TO_SR}}
                for m, v in corpus.items()}
    got = int_view(base)
    results.append(("R1 parse+dedup sanity (base == corpus manifest)",
                    got == expected,
                    "exact match" if got == expected
                    else f"got {got} expected {expected}"))

    # R2 — rewrite retention (the regression this fixture exists for)
    same = int_view(post) == int_view(base) and all(
        abs(post.get(m, {}).get("cost", 0.0) - base.get(m, {}).get("cost", 0.0)) < 1e-9
        for m in set(base) | set(post))
    cost_delta = {m: round(base.get(m, {}).get("cost", 0.0) - post.get(m, {}).get("cost", 0.0), 6)
                  for m in set(base) | set(post)
                  if abs(base.get(m, {}).get("cost", 0.0) - post.get(m, {}).get("cost", 0.0)) >= 1e-9}
    results.append(("R2 rewrite retention (post == base)", same,
                    "totals unchanged" if same
                    else f"drifted: {diff(int_view(base), int_view(post))} cost_delta: {cost_delta}"))

    # R3 — restart stability
    blobs = [(w / f"stab-{i}.json").read_bytes() for i in (1, 2, 3)]
    ok = all(b == blobs[0] for b in blobs)
    results.append(("R3 restart stability (3 runs byte-identical)", ok,
                    "byte-identical" if ok else "outputs differ"))

    # R4 — drift demonstration on a pre-fix baseline binary
    if args.baseline:
        drift = json.loads((w / "drift-manifest.json").read_text())["expected_delta_by_model"]
        b_old = int_view(cc_by_model(w / "base-baseline.json"))
        p_old = int_view(cc_by_model(w / "post-baseline.json"))
        drop = diff(b_old, p_old)
        matched = None
        for conv, count_key in (("last_line", "messages"), ("sum_lines", "lines")):
            exp = {m: {"messageCount": v[count_key],
                       **{MANIFEST_TO_SR[k]: v[conv][k] for k in MANIFEST_TO_SR}}
                   for m, v in drift.items()}
            if drop == exp:
                matched = conv
                break
        results.append(("R4 baseline drift demo (old bin drops removed usage)",
                        matched is not None,
                        f"matches ({matched})" if matched
                        else f"drop {drop} vs manifest {drift}"))

    width = max(len(n) for n, _, _ in results)
    all_ok = True
    for name, ok, detail in results:
        all_ok &= ok
        print(f"{'PASS' if ok else 'FAIL'}  {name:<{width}}  {detail}")
    print("RESULT:", "ALL PASS" if all_ok else "FAILURES")
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
