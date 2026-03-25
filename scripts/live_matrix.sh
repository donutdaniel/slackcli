#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli_bin="${SLACKCLI_BIN:-$repo_root/target/debug/slackcli}"
scope="${SLACKCLI_TEST_SCOPE:-matrix}"
profile="${SLACKCLI_PROFILE:-}"
prefix="${SLACKCLI_SMOKE_CHANNEL_PREFIX:-slackcli-test}"
channel_name_override="${SLACKCLI_SMOKE_CHANNEL_NAME:-}"
channel_id="${SLACK_TEST_CHANNEL_ID:-}"
allow_post="${SLACK_TEST_ALLOW_POST:-1}"
enable_search="${SLACKCLI_MATRIX_ENABLE_SEARCH:-1}"
enable_workspace_reads="${SLACKCLI_MATRIX_ENABLE_WORKSPACE_READS:-1}"

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

channel_name=""
message_ts=""
reply_ts=""
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

  if [[ -n "$reply_ts" && -n "$channel_id" ]]; then
    echo "== cleanup reply delete =="
    "${cmd_base[@]}" message delete "$channel_id" "$reply_ts" | jq . || true
  fi

  if [[ -n "$message_ts" && -n "$channel_id" ]]; then
    echo "== cleanup message delete =="
    "${cmd_base[@]}" message delete "$channel_id" "$message_ts" | jq . || true
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

wait_for_search_result() {
  local query="$1"
  local attempts=5
  local sleep_seconds=2

  for _ in $(seq 1 "$attempts"); do
    local output
    if ! output="$("${cmd_base[@]}" search messages "$query" --count 5)"; then
      echo "$output" >&2
      return 1
    fi

    if echo "$output" | jq -e '.messages.total > 0 or .messages.pagination.total_count > 0' >/dev/null 2>&1; then
      echo "$output" | jq .
      return 0
    fi

    sleep "$sleep_seconds"
  done

  echo "$output" | jq .
  return 1
}

wait_for_file_share() {
  local file_id="$1"
  local expected_channel_id="$2"
  local attempts=5
  local sleep_seconds=2
  local output=""

  for _ in $(seq 1 "$attempts"); do
    if ! output="$("${cmd_base[@]}" file get "$file_id")"; then
      echo "$output" >&2
      return 1
    fi

    if echo "$output" | jq -e --arg channel_id "$expected_channel_id" '
      (.file.channels // [] | index($channel_id)) != null
      or (.file.groups // [] | index($channel_id)) != null
      or (.file.ims // [] | index($channel_id)) != null
      or (
        .file.shares.private // {}
        | to_entries
        | map(.key)
        | index($channel_id)
      ) != null
      or (
        .file.shares.public // {}
        | to_entries
        | map(.key)
        | index($channel_id)
      ) != null
    ' >/dev/null 2>&1; then
      echo "$output" | jq .
      return 0
    fi

    sleep "$sleep_seconds"
  done

  echo "$output" | jq .
  return 1
}

echo "scope: $scope"
echo "binary: $cli_bin"
if [[ -n "$profile" ]]; then
  echo "profile: $profile"
fi

auth_output="$("${cmd_base[@]}" auth whoami)"
echo "== auth whoami =="
echo "$auth_output" | jq .
actor_id="$(echo "$auth_output" | jq -r '.auth_test.user_id // .auth_test.bot_id // empty' | tr '[:upper:]' '[:lower:]')"
self_user_id="$(echo "$auth_output" | jq -r '.auth_test.user_id // empty')"

run_json "auth doctor" auth doctor
run_json "team info" team info
run_json "user me" user me
if [[ -n "$self_user_id" ]]; then
  run_json "user get $self_user_id" user get "$self_user_id"
fi
if [[ "$enable_workspace_reads" == "1" ]]; then
  run_json "user list" user list --limit 5
fi

if [[ -n "$channel_id" ]]; then
  echo "using explicit test channel id: $channel_id"
elif [[ -n "$channel_name_override" ]]; then
  channel_name="$channel_name_override"
  echo "using deterministic test channel name: $channel_name"
  channel_id="$(lookup_channel_id "#$channel_name" || true)"
  if [[ -z "$channel_id" ]]; then
    created="$("${cmd_base[@]}" conversation create "$channel_name" --private)"
    echo "== conversation create $channel_name =="
    echo "$created" | jq .
    channel_id="$(echo "$created" | jq -r '.channel.id // empty')"
  fi
else
  if [[ -z "$actor_id" ]]; then
    echo "cannot derive per-user matrix test channel name without an auth user id; set SLACK_TEST_CHANNEL_ID or SLACKCLI_SMOKE_CHANNEL_NAME" >&2
    exit 1
  fi
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
  echo "failed to resolve or create matrix test channel" >&2
  exit 1
fi

info_output="$("${cmd_base[@]}" conversation info "$channel_id")"
echo "== conversation info $channel_id =="
echo "$info_output" | jq .
if [[ -z "$channel_name" ]]; then
  channel_name="$(echo "$info_output" | jq -r '.channel.name // empty')"
fi
run_json "conversation members $channel_id" conversation members "$channel_id" --limit 10
run_json "conversation history $channel_id" conversation history "$channel_id" --limit 5
run_json "api call auth.test" api call auth.test
run_json "api call conversations.info" api call conversations.info --http get --param "channel=$channel_id"
if [[ "$enable_workspace_reads" == "1" ]]; then
  if [[ -n "$self_user_id" ]]; then
    run_json "conversation list private_channel" conversation list --types private_channel --exclude-archived --user "$self_user_id" --limit 10
  else
    run_json "conversation list private_channel" conversation list --types private_channel --exclude-archived --limit 10
  fi
fi

if [[ "$scope" == "smoke" ]]; then
  exit 0
fi

if [[ "$allow_post" != "1" ]]; then
  echo "skipping write checks: set SLACK_TEST_ALLOW_POST=1 to enable matrix message/thread/file exercises"
  exit 0
fi

nonce="$(date +%s)-$$"
sent="$("${cmd_base[@]}" message send "$channel_id" --text "slackcli matrix ${nonce}")"
echo "== message send =="
echo "$sent" | jq .
message_ts="$(echo "$sent" | jq -r '.ts // empty')"

if [[ -z "$message_ts" ]]; then
  echo "message send did not return a ts; cannot continue matrix checks" >&2
  exit 1
fi

reply="$("${cmd_base[@]}" message send "$channel_id" --text "slackcli matrix reply ${nonce}" --thread-ts "$message_ts")"
echo "== message thread reply =="
echo "$reply" | jq .
reply_ts="$(echo "$reply" | jq -r '.ts // empty')"

run_json "conversation replies $channel_id" conversation replies "$channel_id" "$message_ts" --limit 5
run_json "message update" message update "$channel_id" "$message_ts" --text "slackcli matrix updated ${nonce}"
run_json "message permalink" message permalink "$channel_id" "$message_ts"
run_json "reaction add" reaction add "$channel_id" "$message_ts" eyes
if [[ "$enable_workspace_reads" == "1" ]]; then
  if [[ -n "$self_user_id" ]]; then
    run_json "reaction list" reaction list --user "$self_user_id" --count 5
  else
    run_json "reaction list" reaction list --count 5
  fi
fi
run_json "reaction remove" reaction remove "$channel_id" "$message_ts" eyes

tmp_file="$(mktemp -t slackcli-matrix.XXXXXX)"
printf 'slackcli matrix %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$tmp_file"
uploaded="$("${cmd_base[@]}" file upload "$tmp_file" --channel "$channel_id" --title "slackcli matrix" --initial-comment "slackcli matrix file ${nonce}")"
echo "== file upload =="
echo "$uploaded" | jq .
file_id="$(echo "$uploaded" | jq -r '.files[0].id // .file.id // .files[0].file.id // empty')"

if [[ -n "$file_id" ]]; then
  echo "== file get =="
  wait_for_file_share "$file_id" "$channel_id"
fi
if [[ "$enable_workspace_reads" == "1" ]]; then
  if [[ -n "$self_user_id" ]]; then
    run_json "file list --user $self_user_id" file list --user "$self_user_id" --count 10
  else
    run_json "file list" file list --count 10
  fi
fi
run_json "api call chat.getPermalink" api call chat.getPermalink --http get --param "channel=$channel_id" --param "message_ts=$message_ts"

if [[ "$enable_search" == "1" ]]; then
  search_query="\"slackcli matrix updated ${nonce}\""
  if [[ -n "$channel_name" ]]; then
    search_query="${search_query} in:${channel_name}"
  fi
  echo "== search messages $search_query =="
  wait_for_search_result "$search_query" || echo "search did not return a match within the retry window" >&2
fi
