#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def slugify_channel_name(raw: str) -> str:
    lowered = raw.strip().lower()
    slug = re.sub(r"[^a-z0-9_-]+", "-", lowered)
    slug = re.sub(r"-{2,}", "-", slug).strip("-")
    return slug[:80] or "slackcli-bench"


def run_cli(cli_bin: Path, *args: str) -> dict[str, Any]:
    command = [str(cli_bin), "--output", "json", *args]
    completed = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
        env=os.environ.copy(),
    )
    if completed.returncode != 0:
        message = completed.stderr.strip() or completed.stdout.strip()
        raise RuntimeError(f"command failed ({completed.returncode}): {' '.join(command)}\n{message}")
    return json.loads(completed.stdout)


def wait_for_search(cli_bin: Path, query: str, timeout_seconds: int, poll_seconds: int) -> None:
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        payload = run_cli(cli_bin, "search", "messages", query, "--count", "1")
        messages = payload.get("messages", {})
        total = messages.get("total")
        if isinstance(total, int) and total > 0:
            return
        pagination = messages.get("pagination", {})
        total_count = pagination.get("total_count")
        if isinstance(total_count, int) and total_count > 0:
            return
        time.sleep(poll_seconds)
    raise TimeoutError(f"timed out waiting for benchmark search fixture to index: {query}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Create a disposable Slack benchmark fixture channel.")
    parser.add_argument(
        "--cli-bin",
        type=Path,
        default=Path("./target/release/slackcli"),
        help="Path to the slackcli binary",
    )
    parser.add_argument(
        "--format",
        choices=("json", "env"),
        default="json",
        help="Output format",
    )
    parser.add_argument(
        "--prefix",
        required=True,
        help="Prefix used to derive the fixture channel name",
    )
    parser.add_argument(
        "--search-timeout-seconds",
        type=int,
        default=90,
        help="How long to wait for the seeded read message to become searchable",
    )
    parser.add_argument(
        "--search-poll-seconds",
        type=int,
        default=2,
        help="How frequently to poll Slack search while seeding the fixture",
    )
    parser.add_argument(
        "--skip-search-wait",
        action="store_true",
        help="Do not wait for the seeded read message to become searchable",
    )
    parser.add_argument(
        "--settle-seconds",
        type=int,
        default=3,
        help="How long to wait after seeding the channel before returning the fixture",
    )
    args = parser.parse_args()

    nonce = str(time.time_ns())
    channel_name = slugify_channel_name(f"{args.prefix}-{nonce}")
    created = run_cli(args.cli_bin, "conversation", "create", channel_name, "--private")
    channel = created.get("channel", {})
    channel_id = channel.get("id")
    if not isinstance(channel_id, str) or not channel_id:
        raise RuntimeError("conversation create did not return a channel id")

    root_text = f"slackcli bench root {nonce}"
    reply_text = f"slackcli bench reply {nonce}"
    whoami = run_cli(args.cli_bin, "auth", "whoami")
    self_user_id = (
        whoami.get("auth_test", {}).get("user_id")
        if isinstance(whoami, dict)
        else None
    )
    if not isinstance(self_user_id, str) or not self_user_id:
        raise RuntimeError("auth whoami did not return a user_id")
    sent = run_cli(args.cli_bin, "message", "send", channel_id, "--text", root_text)
    root_ts = sent.get("ts")
    if not isinstance(root_ts, str) or not root_ts:
        raise RuntimeError("message send did not return a root ts")

    replied = run_cli(
        args.cli_bin,
        "message",
        "send",
        channel_id,
        "--text",
        reply_text,
        "--thread-ts",
        root_ts,
    )
    reply_ts = replied.get("ts")
    if not isinstance(reply_ts, str) or not reply_ts:
        raise RuntimeError("message send did not return a reply ts")

    if args.settle_seconds > 0:
        time.sleep(args.settle_seconds)

    # Keep the seeded search query unique via the nonce-bearing root text.
    # Omitting a channel filter has proven more reliable for Slack indexing.
    search_query = f"\"{root_text}\""
    if not args.skip_search_wait:
        wait_for_search(
            args.cli_bin,
            search_query,
            timeout_seconds=max(args.search_timeout_seconds, 1),
            poll_seconds=max(args.search_poll_seconds, 1),
        )

    payload = {
        "SLACK_FIXTURE_CHANNEL_ID": channel_id,
        "SLACK_FIXTURE_CHANNEL_NAME": channel_name,
        "SLACK_FIXTURE_ROOT_TS": root_ts,
        "SLACK_FIXTURE_REPLY_TS": reply_ts,
        "SLACK_FIXTURE_SEARCH_QUERY": search_query,
        "SLACK_FIXTURE_SELF_USER_ID": self_user_id,
    }

    if args.format == "env":
        for key, value in payload.items():
            print(f"{key}={value}")
        return

    print(json.dumps(payload, indent=2, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
