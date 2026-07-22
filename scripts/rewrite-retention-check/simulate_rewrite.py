#!/usr/bin/env python3
"""Simulate a Claude Code resume/compact rewrite on a (disposable) tree.

Picks the largest main transcript (``<slug>/<sessionId>.jsonl``), removes ALL
lines belonging to the last N distinct assistant ``message.id`` groups, and
rewrites the file in place — the observable effect of a real resume/compact:
messages vanish, mtime bumps (see Piebald-AI/splitrail#200).

The manifest records the expected per-model delta under both streaming
conventions, so the checker stays agnostic to how partial lines are counted:

- ``last_line`` : one record per message.id, usage of its last line
- ``sum_lines`` : one record per line, usage summed

Usage: simulate_rewrite.py ROOT [--drop 3] [--manifest out.json]
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

USAGE_KEYS = ("input_tokens", "output_tokens",
              "cache_read_input_tokens", "cache_creation_input_tokens")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("root", type=Path, help="projects root (will be modified!)")
    ap.add_argument("--drop", type=int, default=3)
    ap.add_argument("--manifest", type=Path, default=Path("drift-manifest.json"))
    args = ap.parse_args()

    mains = sorted(args.root.glob("*/*.jsonl"),
                   key=lambda p: p.stat().st_size, reverse=True)
    if not mains:
        print("no main transcripts under", args.root, file=sys.stderr)
        return 1
    target = mains[0]

    lines = target.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True)
    order: list[str] = []
    by_id: dict[str, list[dict]] = {}
    for raw in lines:
        if '"assistant"' not in raw:
            continue
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if rec.get("type") != "assistant":
            continue
        msg = rec.get("message") or {}
        mid = msg.get("id")
        if not mid or not isinstance(msg.get("usage"), dict):
            continue
        if mid not in by_id:
            order.append(mid)
            by_id[mid] = []
        by_id[mid].append(msg)

    if len(order) <= args.drop:
        print(f"only {len(order)} messages in target; need > {args.drop}", file=sys.stderr)
        return 1
    drop_ids = set(order[-args.drop:])

    expected: dict[str, dict] = {}
    for mid in drop_ids:
        msgs = by_id[mid]
        model = msgs[-1].get("model") or "unknown"
        slot = expected.setdefault(model, {
            "messages": 0, "lines": 0,
            "last_line": {k: 0 for k in USAGE_KEYS},
            "sum_lines": {k: 0 for k in USAGE_KEYS},
        })
        slot["messages"] += 1
        slot["lines"] += len(msgs)
        for k in USAGE_KEYS:
            slot["last_line"][k] += int((msgs[-1].get("usage") or {}).get(k) or 0)
            for m in msgs:
                slot["sum_lines"][k] += int((m.get("usage") or {}).get(k) or 0)

    kept, removed = [], 0
    for raw in lines:
        keep = True
        if '"assistant"' in raw:
            try:
                rec = json.loads(raw)
                if rec.get("type") == "assistant" and \
                        (rec.get("message") or {}).get("id") in drop_ids:
                    keep = False
            except json.JSONDecodeError:
                pass
        if keep:
            kept.append(raw)
        else:
            removed += 1
    target.write_text("".join(kept), encoding="utf-8")

    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    args.manifest.write_text(json.dumps({
        "target_file": str(target),
        "dropped_message_ids": sorted(drop_ids),
        "removed_lines": removed,
        "expected_delta_by_model": expected,
    }, indent=2), encoding="utf-8")
    print(f"rewrote {target.name}: -{len(drop_ids)} messages ({removed} lines)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
