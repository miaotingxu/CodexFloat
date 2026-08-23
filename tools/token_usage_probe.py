#!/usr/bin/env python
"""Privacy-safe Codex JSONL token usage probe for Codex Float M0."""
from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any


def parse_timestamp(value: Any) -> datetime | None:
    try:
        return datetime.fromisoformat(str(value).replace("Z", "+00:00")).astimezone()
    except (TypeError, ValueError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sessions", type=Path, default=Path.home() / ".codex" / "sessions")
    args = parser.parse_args()
    files = list(args.sessions.rglob("*.jsonl")) if args.sessions.exists() else []
    daily: Counter[str] = Counter()
    daily_by_origin: dict[str, Counter[str]] = defaultdict(Counter)
    origins: Counter[str] = Counter()
    stats: Counter[str] = Counter()

    for path in files:
        origin = "unknown"
        previous_total: int | None = None
        has_token_event = False
        for line in path.read_text(encoding="utf-8", errors="ignore").splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                stats["invalid_json_lines"] += 1
                continue
            payload = event.get("payload")
            if event.get("type") == "session_meta" and isinstance(payload, dict):
                origin = str(payload.get("originator") or "unknown")
                continue
            if not (event.get("type") == "event_msg" and isinstance(payload, dict) and payload.get("type") == "token_count"):
                continue
            info = payload.get("info")
            if not isinstance(info, dict):
                stats["token_events_without_info"] += 1
                continue
            total_usage = info.get("total_token_usage")
            last_usage = info.get("last_token_usage")
            total = total_usage.get("total_tokens") if isinstance(total_usage, dict) else None
            last = last_usage.get("total_tokens") if isinstance(last_usage, dict) else None
            if not isinstance(total, int) or not isinstance(last, int):
                stats["token_events_without_totals"] += 1
                continue
            stats["token_events"] += 1
            has_token_event = True
            if previous_total is not None and total == previous_total:
                stats["duplicate_snapshots_ignored"] += 1
                continue
            if previous_total is not None and total < previous_total:
                stats["counter_resets_handled"] += 1
            timestamp = parse_timestamp(event.get("timestamp"))
            if timestamp is None:
                stats["invalid_token_timestamps"] += 1
                previous_total = total
                continue
            day = timestamp.date().isoformat()
            daily[day] += last
            daily_by_origin[origin][day] += last
            previous_total = total
        origins[origin] += 1
        if has_token_event:
            stats["files_with_token_events"] += 1

    today = datetime.now().astimezone().date()
    def sum_days(days: int) -> int:
        return sum(daily[(today - timedelta(days=offset)).isoformat()] for offset in range(days))

    result = {
        "probe": "codex-jsonl-token-usage",
        "sessions_root": str(args.sessions),
        "session_files": len(files),
        "originator_file_counts": dict(origins),
        "stats": dict(stats),
        "usage": {
            "today": daily[today.isoformat()],
            "last_7_days": sum_days(7),
            "last_30_days": sum_days(30),
            "today_by_originator": {
                origin: counts[today.isoformat()]
                for origin, counts in daily_by_origin.items()
                if counts[today.isoformat()]
            },
            "daily_last_7_days": {
                (today - timedelta(days=offset)).isoformat(): daily[(today - timedelta(days=offset)).isoformat()]
                for offset in reversed(range(7))
            },
        },
        "algorithm": "Sum last_token_usage.total_tokens only when total_token_usage.total_tokens changes in that session file.",
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
