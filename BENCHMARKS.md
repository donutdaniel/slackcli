# Benchmarks

This file captures the latest benchmark snapshot for `slackcli` versus Slack's
hosted MCP server.

Important: this is a one-shot end-to-end latency benchmark from one machine.
Each measurement starts from a fresh local invocation:

- CLI: a fresh `slackcli` process per run
- MCP: a fresh MCP session per run

The numbers include more than just "the network" or "the protocol". They also
include local startup/config loading, request construction, response parsing,
network latency, and remote Slack or MCP service time.

It does not measure a reused-session MCP client, and it does not measure a
persistent long-lived CLI process. This is not a pure protocol benchmark. Read
the results as "one-shot local tool latency from this machine" rather than a
universal claim about every CLI vs MCP integration model.

## Method

- Snapshot date: `2026-03-25 17:04:54 EDT`
- Code state: local uncommitted worktree during the benchmark run
- Machine: `Apple M4 Max`
- OS: `macOS 26.2 (25C56)`
- Runner: `hyperfine 1.20.0`
- CLI binary: `./target/release/slackcli`
- MCP server: `https://mcp.slack.com/mcp`
- Orchestrator: `scripts/bench_strengthen.py`
- Raw artifacts: `benchmarks/publish-20260325-comprehensive/`
- Reported metric: median of session medians from the exported `hyperfine` JSON
  files in `benchmarks/publish-20260325-comprehensive/`
- Backend dependency: live Slack Web API and hosted MCP service
- Transient retry policy: `bench_case_once.py --retries 3 --retry-delay-ms 1000`
- Comparison mode:
  - CLI: fresh process per run
  - MCP: fresh session per run

### Read Suite

- Case file: `scripts/bench_cases.readsuite.json`
- Fixture strategy: one fixed disposable fixture set per session
- Sessions: `2`
- Runs per case: `20`
- Warmup per case: `3`

### Write Suite

- Case file: `scripts/bench_cases.writesuite.json`
- Fixture strategy: one isolated disposable fixture set per case, per session
- Sessions: `2`
- Runs per case: `10`
- Warmup per case: `2`

This snapshot separates reads from writes, isolates mutating cases, and repeats
the suite across multiple sessions.

For the current repo state, this benchmark surface is comprehensive across the
shared `slackcli` and Slack MCP capabilities:

- Read overlap: current user profile, channel read, thread read, message search
- Write overlap: send message to channel, send message to thread

## Read Suite

| Case | CLI median of medians (ms) | MCP median of medians (ms) | Ratio |
|---|---:|---:|---:|
| `read-channel-page1` | 201.8 | 4174.9 | CLI 20.69x faster |
| `read-current-user-profile` | 175.2 | 580.8 | CLI 3.32x faster |
| `read-thread-page1` | 199.2 | 6729.0 | CLI 33.78x faster |
| `search-messages-page1` | 378.9 | 7785.2 | CLI 20.55x faster |

## Write Suite

| Case | CLI median of medians (ms) | MCP median of medians (ms) | Ratio |
|---|---:|---:|---:|
| `send-message-channel` | 260.1 | 4250.3 | CLI 16.34x faster |
| `send-message-thread` | 277.1 | 4409.9 | CLI 15.91x faster |

## Notes

- The CLI was faster in `6/6` cases in this snapshot.
- In this snapshot, the read cases land between `3.32x` and `33.78x` in favor
  of the CLI.
- In this snapshot, the write cases land between `15.91x` and `16.34x` in favor
  of the CLI.
- Slack MCP hit rate limits on the channel, thread, search, and write paths
  during this run, so MCP variance was much higher than CLI variance.
  `read-thread-page1` and `search-messages-page1` were especially noisy across
  sessions, so those large read gaps should be treated cautiously even though
  the directional result is consistent.
- This is still a directional benchmark, not a protocol proof.
- The comparison is between a local Rust CLI calling Slack's public Web API and
  Slack's hosted MCP service.
- The headline result should be read narrowly: in this repo's current setup,
  one-shot CLI invocations were faster than one-shot fresh-session MCP calls.
- A reused-session MCP client may perform differently, and a future persistent
  CLI mode would also change the comparison.
- A different machine, network path, time of day, workspace shape, token scope,
  or Slack-side load can move these numbers around.
