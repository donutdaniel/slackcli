#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


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


def main() -> None:
    parser = argparse.ArgumentParser(description="Archive a disposable Slack benchmark fixture channel.")
    parser.add_argument(
        "--cli-bin",
        type=Path,
        default=Path("./target/release/slackcli"),
        help="Path to the slackcli binary",
    )
    parser.add_argument(
        "--channel-id",
        required=True,
        help="Slack channel id to archive",
    )
    parser.add_argument(
        "--keep",
        action="store_true",
        help="Keep the fixture channel instead of archiving it",
    )
    args = parser.parse_args()

    if args.keep:
        return

    run_cli(args.cli_bin, "conversation", "archive", args.channel_id)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
