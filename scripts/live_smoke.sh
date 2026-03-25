#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli_bin="${SLACKCLI_BIN:-$repo_root/target/debug/slackcli}"
profile="${SLACKCLI_PROFILE:-}"
prefix="${SLACKCLI_SMOKE_CHANNEL_PREFIX:-slackcli-test}"
channel_name_override="${SLACKCLI_SMOKE_CHANNEL_NAME:-}"
channel_id="${SLACK_TEST_CHANNEL_ID:-}"

if [[ ! -x "$cli_bin" ]]; then
  echo "slackcli binary not found at $cli_bin" >&2
  echo "Build it first with: cargo build" >&2
  exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required. Install it first, for example: brew install jq" >&2
  exit 2
fi

cmd_base=("$cli_bin" "--output" "json")
if [[ -n "$profile" ]]; then
  cmd_base+=("--profile" "$profile")
fi

archive_channel=0
message_ts=""
file_id=""
tmp_file=""

cleanup() {
  local status="$1"
  trap - EXIT
  set +e

  if [[ -n "$file_id" ]]; then
    echo "== cleanup file delete =="
    "${cmd_base[@]}" file delete "$file_id" | jq . || true
  fi

  if [[ -n "$message_ts" && -n "$channel_id" ]]; then
    echo "== cleanup message delete =="
    "${cmd_base[@]}" message delete "$channel_id" "$message_ts" | jq . || true
  fi

  if [[ "$archive_channel" == "1" && -n "$channel_id" ]]; then
    echo "== cleanup conversation archive =="
    "${cmd_base[@]}" conversation archive "$channel_id" | jq . || true
  fi

  if [[ -n "$tmp_file" && -f "$tmp_file" ]]; then
    rm -f "$tmp_file"
  fi

  exit "$status"
}

trap 'cleanup "$?"' EXIT

run_json() {
  local label="$1"
  shift
  echo "== $label =="
  local output
  if ! output="$("${cmd_base[@]}" "$@")"; then
    echo "$output" >&2
    return 1
  fi
  echo "$output" | jq .
}

lookup_channel_id() {
  local ref="$1"
  local output
  if ! output="$("${cmd_base[@]}" conversation info "$ref" 2>/dev/null)"; then
    return 1
  fi
  echo "$output" | jq -r '.channel.id // empty'
}

echo "binary: $cli_bin"
if [[ -n "$profile" ]]; then
  echo "profile: $profile"
fi

auth_output="$("${cmd_base[@]}" auth whoami)"
echo "== auth whoami =="
echo "$auth_output" | jq .
actor_id="$(echo "$auth_output" | jq -r '.auth_test.user_id // .auth_test.bot_id // empty' | tr '[:upper:]' '[:lower:]')"
run_json "team info" team info
run_json "user me" user me

if [[ -n "$channel_id" ]]; then
  echo "using explicit test channel id: $channel_id"
elif [[ -n "$channel_name_override" ]]; then
  echo "using deterministic test channel name: $channel_name_override"
  channel_id="$(lookup_channel_id "#$channel_name_override" || true)"
  if [[ -z "$channel_id" ]]; then
    created="$("${cmd_base[@]}" conversation create "$channel_name_override" --private)"
    echo "== conversation create $channel_name_override =="
    echo "$created" | jq .
    channel_id="$(echo "$created" | jq -r '.channel.id // empty')"
    if [[ -z "$channel_id" ]]; then
      echo "conversation create did not return a channel.id" >&2
      exit 1
    fi
  fi
else
  if [[ -z "$actor_id" ]]; then
    channel_name="${prefix}-$(date +%Y%m%d%H%M%S)-$$"
    archive_channel=1
    created="$("${cmd_base[@]}" conversation create "$channel_name" --private)"
    echo "== conversation create $channel_name =="
    echo "$created" | jq .
    channel_id="$(echo "$created" | jq -r '.channel.id // empty')"
  else
    channel_name="${prefix}-${actor_id}"
    echo "using default user-unique test channel name: $channel_name"
    channel_id="$(lookup_channel_id "#$channel_name" || true)"
    if [[ -z "$channel_id" ]]; then
      created="$("${cmd_base[@]}" conversation create "$channel_name" --private)"
      echo "== conversation create $channel_name =="
      echo "$created" | jq .
      channel_id="$(echo "$created" | jq -r '.channel.id // empty')"
    fi
  fi

  if [[ -z "$channel_id" ]]; then
    echo "failed to resolve or create test channel $channel_name" >&2
    exit 1
  fi
fi

run_json "conversation info $channel_id" conversation info "$channel_id"
run_json "conversation members $channel_id" conversation members "$channel_id" --limit 10

nonce="$(date +%s)-$$"
sent="$("${cmd_base[@]}" message send "$channel_id" --text "slackcli smoke ${nonce}")"
echo "== message send =="
echo "$sent" | jq .
message_ts="$(echo "$sent" | jq -r '.ts // empty')"

if [[ -z "$message_ts" ]]; then
  echo "message send did not return a ts" >&2
  exit 1
fi

run_json "message update" message update "$channel_id" "$message_ts" --text "slackcli smoke updated ${nonce}"
run_json "message permalink" message permalink "$channel_id" "$message_ts"
run_json "reaction add" reaction add "$channel_id" "$message_ts" eyes
run_json "reaction remove" reaction remove "$channel_id" "$message_ts" eyes

tmp_file="$(mktemp -t slackcli-smoke.XXXXXX)"
printf 'slackcli smoke %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$tmp_file"
uploaded="$("${cmd_base[@]}" file upload "$tmp_file" --channel "$channel_id" --title "slackcli smoke")"
echo "== file upload =="
echo "$uploaded" | jq .
file_id="$(echo "$uploaded" | jq -r '.files[0].id // .file.id // .files[0].file.id // empty')"

if [[ -n "$file_id" ]]; then
  run_json "file get" file get "$file_id"
fi

run_json "conversation history $channel_id" conversation history "$channel_id" --limit 5
run_json "message delete" message delete "$channel_id" "$message_ts"
message_ts=""

if [[ -n "$file_id" ]]; then
  run_json "file delete" file delete "$file_id"
  file_id=""
fi

if [[ "$archive_channel" == "1" ]]; then
  run_json "conversation archive" conversation archive "$channel_id"
  channel_id=""
fi

if [[ -n "$tmp_file" && -f "$tmp_file" ]]; then
  rm -f "$tmp_file"
  tmp_file=""
fi

echo "isolated smoke completed"
