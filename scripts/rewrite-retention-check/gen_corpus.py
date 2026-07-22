#!/usr/bin/env python3
"""Generate a synthetic Claude Code corpus with a manifest of expected totals.

Writes main transcripts (``<slug>/<sessionId>.jsonl``) under ROOT, which
should be ``<isolated-home>/.claude/projects``.

Realism the check depends on:

- Each assistant API message is written as TWO JSONL lines with distinct line
  ``uuid``s but identical ``message.id`` / ``requestId`` / ``usage`` —
  mirroring Claude Code's streaming duplication — so the scan exercises the
  analyzer's local_hash + token-fingerprint deduplication.
- Skip-cases that must NOT count toward totals: user lines, a ``summary``
  line, a ``file-history-snapshot`` line, and one ``<synthetic>`` api-error
  line per session.

The manifest records expected per-model totals (message counts and the four
token fields). Cost is intentionally excluded — pricing tables change.
"""
from __future__ import annotations

import argparse
import json
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

MODELS = ("claude-opus-4-6", "claude-sonnet-4-20250514")


def line(obj: dict) -> str:
    return json.dumps(obj, separators=(",", ":")) + "\n"


def assistant_lines(ts: datetime, sid: str, model: str, usage: dict) -> list[str]:
    mid = "msg_" + uuid.uuid4().hex[:16]
    rid = "req_" + uuid.uuid4().hex[:16]
    out = []
    for stop in (None, "end_turn"):  # two streaming lines, identical usage
        out.append(line({
            "type": "assistant",
            "timestamp": ts.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
            "cwd": "/home/user/demo",
            "sessionId": sid,
            "uuid": str(uuid.uuid4()),
            "parentUuid": None,
            "requestId": rid,
            "version": "2.1.198",
            "message": {
                "id": mid, "type": "message", "role": "assistant",
                "model": model, "stop_reason": stop,
                "content": [{"type": "text", "text": "ok"}],
                "usage": usage,
            },
        }))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("root", type=Path, help="projects root to create")
    ap.add_argument("--manifest", type=Path, required=True)
    ap.add_argument("--sessions", type=int, default=3)
    ap.add_argument("--messages", type=int, default=12)
    args = ap.parse_args()

    proj = args.root / "-home-user-demo"
    proj.mkdir(parents=True, exist_ok=True)
    base = datetime(2026, 7, 1, 9, 0, 0, tzinfo=timezone.utc)

    expected: dict[str, dict] = {
        m: {"messages": 0, "input_tokens": 0, "output_tokens": 0,
            "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
        for m in MODELS
    }

    for s in range(args.sessions):
        sid = str(uuid.uuid4())
        rows: list[str] = []
        # summary line (skip-case)
        rows.append(line({"type": "summary", "summary": f"Demo session {s}",
                          "leafUuid": str(uuid.uuid4())}))
        # file-history-snapshot line (skip-case)
        rows.append(line({"type": "file-history-snapshot",
                          "messageId": str(uuid.uuid4()), "snapshot": {}}))
        for i in range(args.messages):
            model = MODELS[i % len(MODELS)]
            ts = base + timedelta(days=s % 2, hours=s, minutes=7 * i)
            usage = {
                "input_tokens": 1000 + 137 * s + 7 * i,
                "output_tokens": 200 + 3 * i,
                "cache_read_input_tokens": 5000 + 11 * i,
                "cache_creation_input_tokens": 300 + i,
            }
            # user line before each assistant message (skip-case)
            rows.append(line({
                "type": "user", "timestamp": ts.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
                "sessionId": sid, "uuid": str(uuid.uuid4()),
                "message": {"role": "user", "content": "do the thing"},
            }))
            rows.extend(assistant_lines(ts, sid, model, usage))
            slot = expected[model]
            slot["messages"] += 1
            for k, v in usage.items():
                slot[k] += v
        # one <synthetic> api-error line (skip-case)
        rows.append(line({
            "type": "assistant", "timestamp": base.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
            "sessionId": sid, "uuid": str(uuid.uuid4()), "isApiErrorMessage": True,
            "message": {"role": "assistant", "model": "<synthetic>",
                        "content": [{"type": "text", "text": "error"}]},
        }))
        (proj / f"{sid}.jsonl").write_text("".join(rows), encoding="utf-8")

    args.manifest.parent.mkdir(parents=True, exist_ok=True)
    args.manifest.write_text(json.dumps({"expected_by_model": expected}, indent=2),
                             encoding="utf-8")
    total = sum(v["messages"] for v in expected.values())
    print(f"corpus: {args.sessions} sessions, {total} assistant messages -> {proj}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
