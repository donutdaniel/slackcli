# slackcli

Fast, local-first Rust CLI for the Slack Web API.

Default output is pretty JSON. Use `--output json` or `--output yaml` when you
want stable machine-readable output.

## Install

Pick one:

| Method | Command / steps |
|---|---|
| **Homebrew** (macOS / Linux) | `brew install donutdaniel/homebrew-tap/slackcli` |
| **Binary** ([GitHub Releases](https://github.com/donutdaniel/slackcli/releases)) | Download the archive for your platform, extract it, and move `slackcli` onto your `PATH` |
| **Cargo** (from a local checkout) | Clone this repo, then run `cargo install --path .` from the repo root |

For local development:

```bash
cargo build --release
./target/release/slackcli --help

# or run directly from the repo
cargo run -- --help
```

## Quickstart

1. Create a Slack app from the included manifest.
   - Go to <https://api.slack.com/apps> and choose `From a manifest`.
   - Paste [slack-app-manifest.json](./slack-app-manifest.json).
   - Install the app to your workspace.
   - Copy the `User OAuth Token` (`xoxp-...`) from the `OAuth Tokens` section.
   - The manifest is intentionally scoped to the current first-class commands.
   - Corporate workspaces may require admin approval to create or install the app.
2. Choose an auth mode.

Persist a profile with the hidden token prompt:

```bash
slackcli auth login
```

Persist a profile non-interactively:

```bash
slackcli auth login --token "$SLACK_TOKEN"
```

Use an existing `SLACK_TOKEN` without saving credentials:

```bash
env SLACK_TOKEN="$SLACK_TOKEN" slackcli team info
```

3. Verify the token and active profile:

```bash
slackcli auth doctor
slackcli auth whoami
```

4. Try a few commands:

```bash
slackcli conversation list --types im,mpim
slackcli conversation history '#general' --limit 50
slackcli conversation open @alice
slackcli resolve conversation @alice
slackcli search messages "from:alice deploy failed"
```

For a personal CLI, a user token is usually the most useful option because it
can read the conversations your Slack user can access. A bot token works too,
but it only sees what the bot itself can access.

Shell note: quote references that start with `#`, for example `'#general'`, so
your shell does not treat them as comments.

Most commands accept more than raw Slack IDs:

- conversation refs: `C123`, `D123`, `'#general'`, `general`, `@alice`
- user refs: `U123`, `@alice`, `alice@example.com`, display names

Resolution note:

- read commands do not create or open DMs implicitly
- `conversation open`, `message send @user`, and `file upload --channel @user` may create or resume a DM explicitly
- `resolve conversation @user` only returns an existing DM; it never opens one
- `search messages` passes the query string to Slack unchanged, so Slack's own search syntax and quirks apply

## Global Flags

| Flag | Description |
|---|---|
| `--output <format>` | `human` (default), `json`, or `yaml` |
| `--profile <name>` | Use a specific saved profile instead of the active one |
| `--verbose` | Increase log verbosity. Repeat for more detail |

## Common Commands

Run `slackcli <group> --help` for the full flag surface and more examples.

### auth

```bash
slackcli auth login
slackcli auth login --token "$SLACK_TOKEN"
slackcli auth login --profile-name work
slackcli auth list
slackcli auth use work
slackcli auth logout work
```

### conversation

```bash
slackcli conversation list --types public_channel,private_channel
slackcli conversation open @alice
slackcli conversation open @alice,@bob
slackcli conversation create slackcli-smoke --private
slackcli conversation info '#general'
slackcli conversation history '#general' --limit 100
slackcli conversation replies '#general' 1710000000.000100 --limit 100
slackcli conversation members '#general'
```

### resolve

```bash
slackcli resolve user @alice
slackcli resolve conversation '#general'
slackcli resolve conversation @alice
```

### message and reaction

```bash
slackcli message send '#general' --text "hello"
slackcli message send @alice --text "reply" --thread-ts 1710000000.000100
slackcli message send '#general' --text "plain link" --no-unfurl-links --no-unfurl-media
slackcli message update '#general' 1710000000.000100 --text "updated"
slackcli message delete '#general' 1710000000.000100
slackcli message permalink '#general' 1710000000.000100
slackcli reaction add '#general' 1710000000.000100 thumbsup
slackcli reaction remove '#general' 1710000000.000100 thumbsup
slackcli reaction list --user @alice --full
```

`message send` preserves Slack's default unfurl behavior. Pass
`--no-unfurl-links` and/or `--no-unfurl-media` when you want a clean
no-preview send.

### file, user, team, and search

```bash
slackcli file list --channel '#general' --count 50
slackcli file upload ./report.pdf --channel '#general'
slackcli file upload ./image.png --channel @alice --initial-comment "latest"
slackcli user me
slackcli user get @alice
slackcli team info
slackcli search messages "from:alice in:#eng" --count 50 --sort timestamp --sort-dir desc
```

### api

Use this when a Slack method is not covered by a dedicated command yet. The
bundled manifest stays intentionally minimal for the first-class commands
above, so direct `api call` usage may require adding extra Slack scopes first.

```bash
slackcli api call conversations.list --http get --param types=public_channel
slackcli api call chat.postMessage --param channel=C12345 --param text=hello
slackcli api call views.publish --body-json '{"user_id":"U123","view":{"type":"home","blocks":[]}}'
```

For `GET` requests, repeated `--param key=value` entries become query
parameters. For `POST` requests, they merge into the JSON object body. Use
`--body-json`, `--from-file`, or `--stdin` when you need a richer payload.

## Output

```bash
slackcli conversation list
slackcli --output json conversation history '#general'
slackcli --output yaml team info
```

`human` is the default. Most commands emit the raw Slack API response whenever
possible. CLI-native commands such as `auth list` and `auth doctor` emit
structured wrapper objects instead.

## Coverage

Current first-class coverage, raw API fallback coverage, and notable gaps live
in [COVERAGE.md](./COVERAGE.md).

## Live Testing

Three local scripts exercise a real Slack workspace:

- `scripts/live_smoke.sh`: safe default; keeps writes inside your private test channel
- `scripts/live_matrix.sh`: broader pass with workspace reads plus isolated writes
- `scripts/live_write_smoke.sh`: compatibility alias for `scripts/live_smoke.sh`

Requirements:

- a logged-in profile
- `jq`

Examples:

```bash
cargo build
scripts/live_smoke.sh
scripts/live_matrix.sh
```

If you want the safest smoke path, use `scripts/live_smoke.sh`.

## Benchmarks

This repo includes a CLI-vs-MCP benchmark harness. It compares `slackcli`
against Slack's hosted MCP server using the same saved user token and isolated
private benchmark channels.

See [BENCHMARKS.md](./BENCHMARKS.md) for methodology and current numbers.

Quick example:

```bash
cargo build --release
scripts/hyperfine_bench.sh
python3 scripts/bench_strengthen.py --suite read
```

If you want CLI-vs-MCP comparison, Slack MCP must be enabled for your app
first.

## Slack Docs

Primary references for this CLI:

- Token guidance: <https://docs.slack.dev/authentication/tokens/>
- OAuth install: <https://docs.slack.dev/authentication/installing-with-oauth/>
- Web API reference: <https://docs.slack.dev/reference/methods>
- Slack MCP overview: <https://docs.slack.dev/ai/slack-mcp-server/>
