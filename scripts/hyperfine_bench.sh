#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli_bin="${CLI_BIN:-$repo_root/target/release/slackcli}"
case_file="${CASE_FILE:-$repo_root/scripts/bench_cases.readsuite.json}"
keep_fixture="${BENCH_KEEP_FIXTURE:-0}"
prefix="${BENCH_FIXTURE_PREFIX:-bench-once}"

fixture_file="$(mktemp -t slackcli-bench-fixture.XXXXXX)"
env_file="$(mktemp -t slackcli-bench-env.XXXXXX)"
skip_search_wait="$(
python3 - "$case_file" "$@" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
cases = payload.get("cases") if isinstance(payload, dict) else payload
selected = set(sys.argv[2:])
needs_search = False
if isinstance(cases, list):
    for case in cases:
        if not isinstance(case, dict):
            continue
        name = case.get("name")
        if selected and name not in selected:
            continue
        cli = case.get("cli")
        if isinstance(cli, list) and cli[:2] == ["search", "messages"]:
            needs_search = True
            break
        mcp = case.get("mcp")
        if isinstance(mcp, dict) and mcp.get("alias") == "search_messages":
            needs_search = True
            break
print("0" if needs_search else "1")
PY
)"

cleanup() {
  local status="$1"
  trap - EXIT

  if [[ -f "$fixture_file" ]]; then
    if [[ "$keep_fixture" != "1" ]]; then
      channel_id="$(python3 - "$fixture_file" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.exists() or path.stat().st_size == 0:
    print("")
    raise SystemExit(0)
try:
    payload = json.loads(path.read_text(encoding="utf-8"))
except json.JSONDecodeError:
    print("")
    raise SystemExit(0)
print(payload.get("SLACK_FIXTURE_CHANNEL_ID", ""))
PY
)"
      if [[ -n "$channel_id" ]]; then
        python3 "$repo_root/scripts/bench_cleanup_fixture.py" --cli-bin "$cli_bin" --channel-id "$channel_id" >/dev/null || true
      fi
    fi
    rm -f "$fixture_file"
  fi
  if [[ -f "$env_file" ]]; then
    rm -f "$env_file"
  fi

  exit "$status"
}

trap 'cleanup "$?"' EXIT

python3 "$repo_root/scripts/bench_prepare_fixture.py" \
  --cli-bin "$cli_bin" \
  --format json \
  --prefix "$prefix" \
  --search-timeout-seconds "${BENCH_SEARCH_TIMEOUT_SECONDS:-90}" \
  $(if [[ "$skip_search_wait" == "1" ]]; then printf '%s' "--skip-search-wait"; fi) > "$fixture_file"

if [[ ! -s "$fixture_file" ]]; then
  echo "benchmark fixture setup did not produce any output" >&2
  exit 1
fi

python3 - "$fixture_file" <<'PY' > "$env_file"
import json
import shlex
import sys
from pathlib import Path

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
for key, value in payload.items():
    print(f"export {key}={shlex.quote(str(value))}")
PY

# shellcheck disable=SC1090
source "$env_file"

OUT_DIR="${OUT_DIR:-$repo_root/benchmarks/latest}" \
CASE_FILE="$case_file" \
CLI_BIN="$cli_bin" \
"$repo_root/scripts/hyperfine_compare.sh" "$@"
