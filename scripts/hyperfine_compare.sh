#!/usr/bin/env bash
set -euo pipefail

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "hyperfine is required. Install it first, for example: brew install hyperfine" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case_file="${CASE_FILE:-$repo_root/scripts/bench_cases.readsuite.json}"
cli_bin="${CLI_BIN:-$repo_root/target/release/slackcli}"
out_dir="${OUT_DIR:-$repo_root/tmp/hyperfine}"
runs="${RUNS:-20}"
warmup="${WARMUP:-3}"
extra_args="${HYPERFINE_ARGS:-}"
retry_count="${BENCH_CASE_RETRIES:-3}"
retry_delay_ms="${BENCH_CASE_RETRY_DELAY_MS:-1000}"
systems_csv="${BENCH_SYSTEMS:-cli,mcp}"

mkdir -p "$out_dir"

IFS=',' read -r -a systems <<< "$systems_csv"
if [[ "${#systems[@]}" -eq 0 ]]; then
  echo "BENCH_SYSTEMS must contain at least one of: cli,mcp" >&2
  exit 2
fi

for system in "${systems[@]}"; do
  if [[ "$system" != "cli" && "$system" != "mcp" ]]; then
    echo "unsupported benchmark system: $system" >&2
    echo "BENCH_SYSTEMS must contain only: cli,mcp" >&2
    exit 2
  fi
done

if [[ $# -eq 0 ]]; then
  cases=()
  while IFS= read -r line; do
    [[ -n "$line" ]] && cases+=("$line")
  done < <(
    python3 - "$case_file" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
cases = payload.get("cases") if isinstance(payload, dict) else payload
for case in cases:
    if isinstance(case, dict) and isinstance(case.get("name"), str):
        print(case["name"])
PY
  )
else
  cases=("$@")
fi

for case_name in "${cases[@]}"; do
  echo "== $case_name =="
  printf -v cli_cmd 'python3 %q --system cli --case-file %q --case-name %q --cli-bin %q --retries %q --retry-delay-ms %q >/dev/null' \
    "$repo_root/scripts/bench_case_once.py" "$case_file" "$case_name" "$cli_bin" "$retry_count" "$retry_delay_ms"
  printf -v mcp_cmd 'python3 %q --system mcp --case-file %q --case-name %q --retries %q --retry-delay-ms %q >/dev/null' \
    "$repo_root/scripts/bench_case_once.py" "$case_file" "$case_name" "$retry_count" "$retry_delay_ms"

  hyperfine_args=(
    --warmup "$warmup"
    --runs "$runs"
    --export-json "$out_dir/$case_name.json"
  )
  # shellcheck disable=SC2206
  extra_args_array=($extra_args)
  if [[ "${#extra_args_array[@]}" -gt 0 ]]; then
    hyperfine_args+=("${extra_args_array[@]}")
  fi
  for system in "${systems[@]}"; do
    case "$system" in
      cli)
        hyperfine_args+=(-n cli "$cli_cmd")
        ;;
      mcp)
        hyperfine_args+=(-n mcp "$mcp_cmd")
        ;;
    esac
  done

  # shellcheck disable=SC2086
  hyperfine "${hyperfine_args[@]}"
done
