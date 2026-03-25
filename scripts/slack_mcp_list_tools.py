#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import sys

from slack_mcp_common import DEFAULT_MCP_SERVER_URL, SlackMcpClient, load_token


def main() -> None:
    parser = argparse.ArgumentParser(description="List tools exposed by the Slack MCP server.")
    parser.add_argument(
        "--server-url",
        default=DEFAULT_MCP_SERVER_URL,
        help=f"Slack MCP server URL (default: {DEFAULT_MCP_SERVER_URL})",
    )
    parser.add_argument(
        "--format",
        choices=("json", "names"),
        default="names",
        help="Output format",
    )
    args = parser.parse_args()

    client = SlackMcpClient(server_url=args.server_url, access_token=load_token())
    client.initialize()
    tools = client.list_tools()

    if args.format == "json":
        print(json.dumps(tools, indent=2, sort_keys=True))
        return

    for tool in tools:
        name = tool.get("name", "<unknown>")
        description = tool.get("description", "")
        print(f"{name}\t{description}")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
