#!/usr/bin/env python
"""Privacy-safe Codex app-server M0 probe.

Starts the local Codex app-server over stdio, calls only read-only account
methods, and prints an allow-listed JSON result. It never prints email,
auth tokens, cookies, conversation text, or raw protocol streams.
"""
from __future__ import annotations

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


def find_codex() -> Path:
    override = os.environ.get("CODEX_EXE")
    candidates: list[Path] = []
    if override:
        candidates.append(Path(override))
    npm_root = Path(os.environ.get("APPDATA", "")) / "npm" / "node_modules"
    candidates.extend(
        [
            Path(r"D:\nvm4w\nodejs\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"),
            npm_root / "@openai" / "codex" / "node_modules" / "@openai" / "codex-win32-x64" / "vendor" / "x86_64-pc-windows-msvc" / "bin" / "codex.exe",
        ]
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    raise FileNotFoundError("Native codex.exe not found; set CODEX_EXE")


def reader(stream: Any, output: queue.Queue[dict[str, Any]]) -> None:
    for line in stream:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(message, dict):
            output.put(message)


def send(process: subprocess.Popen[str], message: dict[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
    process.stdin.flush()


def await_id(messages: queue.Queue[dict[str, Any]], request_id: int, timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            message = messages.get(timeout=max(0.05, deadline - time.monotonic()))
        except queue.Empty:
            break
        if message.get("id") == request_id:
            return message
    raise TimeoutError(f"app-server request {request_id} timed out")


def safe_account(result: Any) -> dict[str, Any]:
    result = result if isinstance(result, dict) else {}
    account = result.get("account") if isinstance(result.get("account"), dict) else {}
    return {
        "requires_openai_auth": result.get("requiresOpenaiAuth"),
        "account_type": account.get("type"),
        "plan_type": account.get("planType"),
    }


def safe_window(value: Any) -> dict[str, Any] | None:
    if not isinstance(value, dict):
        return None
    return {
        "used_percent": value.get("usedPercent"),
        "remaining_percent": None if value.get("usedPercent") is None else 100 - value["usedPercent"],
        "window_minutes": value.get("windowDurationMins"),
        "resets_at": value.get("resetsAt"),
    }


def safe_rate_limits(result: Any) -> dict[str, Any]:
    result = result if isinstance(result, dict) else {}
    snapshot = result.get("rateLimits") if isinstance(result.get("rateLimits"), dict) else {}
    by_id = result.get("rateLimitsByLimitId") if isinstance(result.get("rateLimitsByLimitId"), dict) else {}
    return {
        "limit_id": snapshot.get("limitId"),
        "plan_type": snapshot.get("planType"),
        "primary": safe_window(snapshot.get("primary")),
        "secondary": safe_window(snapshot.get("secondary")),
        "available_limit_ids": sorted(str(key) for key in by_id),
    }


def safe_usage(result: Any) -> dict[str, Any]:
    result = result if isinstance(result, dict) else {}
    summary = result.get("summary") if isinstance(result.get("summary"), dict) else {}
    buckets = result.get("dailyUsageBuckets")
    safe_buckets = []
    if isinstance(buckets, list):
        for bucket in buckets:
            if isinstance(bucket, dict):
                safe_buckets.append({"start_date": bucket.get("startDate"), "tokens": bucket.get("tokens")})
    return {
        "summary": {
            "lifetime_tokens": summary.get("lifetimeTokens"),
            "peak_daily_tokens": summary.get("peakDailyTokens"),
            "current_streak_days": summary.get("currentStreakDays"),
            "longest_streak_days": summary.get("longestStreakDays"),
        },
        "daily_usage_buckets": safe_buckets,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--timeout", type=float, default=20.0)
    args = parser.parse_args()
    codex = find_codex()
    process = subprocess.Popen(
        [str(codex), "app-server", "--stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,
    )
    assert process.stdout is not None
    messages: queue.Queue[dict[str, Any]] = queue.Queue()
    threading.Thread(target=reader, args=(process.stdout, messages), daemon=True).start()
    try:
        send(process, {"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "codex-float-probe", "title": "Codex Float Probe", "version": "0.1.0"}}})
        initialized = await_id(messages, 1, args.timeout)
        if "error" in initialized:
            raise RuntimeError(f"initialize failed: {initialized['error'].get('message', 'unknown error')}")
        send(process, {"method": "initialized"})
        requests = {
            2: ("account/read", {"refreshToken": False}),
            3: ("account/rateLimits/read", None),
            4: ("account/usage/read", None),
        }
        for request_id, (method, params) in requests.items():
            message: dict[str, Any] = {"id": request_id, "method": method}
            if params is not None:
                message["params"] = params
            send(process, message)
        responses = {request_id: await_id(messages, request_id, args.timeout) for request_id in requests}
        for request_id, response in responses.items():
            if "error" in response:
                raise RuntimeError(f"{requests[request_id][0]} failed: {response['error'].get('message', 'unknown error')}")
        output = {
            "probe": "codex-app-server",
            "codex_executable": codex.name,
            "account": safe_account(responses[2].get("result")),
            "rate_limits": safe_rate_limits(responses[3].get("result")),
            "account_usage": safe_usage(responses[4].get("result")),
        }
        print(json.dumps(output, ensure_ascii=False, indent=2))
        return 0
    except Exception as exc:
        print(json.dumps({"probe": "codex-app-server", "error": str(exc)}, ensure_ascii=False, indent=2), file=sys.stderr)
        return 1
    finally:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()


if __name__ == "__main__":
    raise SystemExit(main())
