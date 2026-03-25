# COVERAGE

This file summarizes the current first-class Slack Web API coverage in
`slackcli`.

The CLI is intentionally opinionated:

- common personal Slack workflows get dedicated commands
- everything else falls back to `slackcli api call`
- the bundled app manifest is scoped to the first-class commands below

When a feature is not listed here, assume there is no dedicated command yet and
use the raw API path instead.

## First-Class Coverage

| Area | CLI coverage | Primary Slack methods |
|---|---|---|
| Auth | `auth login`, `auth doctor`, `auth whoami`, `auth list`, `auth use`, `auth logout` | `auth.test` plus local profile storage |
| Conversations | `conversation list`, `open`, `create`, `archive`, `info`, `history`, `replies`, `members` | `users.conversations`, `conversations.open`, `conversations.create`, `conversations.archive`, `conversations.info`, `conversations.history`, `conversations.replies`, `conversations.members` |
| Messages | `message send`, `update`, `delete`, `permalink` | `chat.postMessage`, `chat.update`, `chat.delete`, `chat.getPermalink` |
| Reactions | `reaction add`, `remove`, `list` | `reactions.add`, `reactions.remove`, `reactions.list` |
| Files | `file list`, `get`, `delete`, `upload` | `files.list`, `files.info`, `files.delete`, `files.getUploadURLExternal`, `files.completeUploadExternal` |
| Users | `user me`, `list`, `get` | `users.info`, `users.list` |
| Team | `team info` | `team.info` |
| Search | `search messages` | `search.messages` |
| Resolve | `resolve user`, `resolve conversation` | local resolution on top of `users.list`, `users.info`, `users.conversations`, and `conversations.info` |
| Raw fallback | `api call` | arbitrary Slack Web API methods |

## Notes By Area

### Auth

- `auth login` validates the token before saving it locally.
- saved profiles are a CLI concept, not a Slack concept
- `auth doctor` and `auth whoami` are thin wrappers around live auth checks

### Conversations

- refs accept IDs, `#channel` names, bare channel names, and `@user` where it
  is safe to support them
- read paths do not implicitly create DMs
- `conversation open`, `message send @user`, and `file upload --channel @user`
  may create or resume a DM explicitly
- `resolve conversation @user` only returns an existing DM and never opens one

### Messages

- thread replies are supported via `--thread-ts`
- `message send` preserves Slack's default unfurl behavior
- use `--no-unfurl-links` and `--no-unfurl-media` when you want a plain link
  post without Slack previews

### Files

- upload uses Slack's external upload flow, not the legacy multipart flow
- uploads can optionally be shared into a conversation and thread in the same
  command

### Search

- only message search has a first-class wrapper today
- the query string is passed to Slack unchanged, so Slack search syntax and
  Slack quirks apply directly

### Resolve

- resolve commands exist to turn human-friendly refs into canonical Slack IDs
  without mutating workspace state
- this is useful for scripts that need deterministic IDs before making write
  calls

### Raw API Fallback

- `slackcli api call` is the escape hatch for any method not covered above
- repeated `--param key=value` entries become query params for `GET`
- repeated `--param key=value` entries merge into the JSON body for `POST`
- `--body-json`, `--from-file`, and `--stdin` are available for richer payloads
- if you use methods outside the bundled manifest's scope set, Slack will
  return scope errors until the app is expanded

## Intentional Gaps

There is no first-class coverage yet for:

- channel rename, join, leave, invite, kick, topic, and purpose management
- scheduled messages and message metadata workflows
- pins, bookmarks, stars, reminders, emoji, and saved items
- canvases, lists, workflows, functions, shortcuts, and Socket Mode
- modals, Home tabs, views, and interactive surfaces
- admin, SCIM, audit, and Enterprise Grid APIs
- user group management
- file comments, remote files, and advanced file administration
- search wrappers beyond message search
- presence, status, huddles, and calls
- app manifests, installs, OAuth flows, and token rotation

Most of these remain reachable through `api call` if the token has the required
scopes and the app is configured appropriately.

## Manifest Scope Boundary

The bundled manifest is intentionally narrow. It is designed to support the
current first-class command set:

- conversations read/write
- history read access
- user lookup
- team lookup
- message search
- chat write
- reactions read/write
- files read/write

If you expand the CLI into more Slack product areas, expect to add scopes to
the manifest before those methods will work reliably through `api call`.

## Practical Rule

Use a first-class command when one exists. Use `api call` when:

- the CLI does not have a dedicated wrapper yet
- you need a method-specific field that the wrapper does not expose
- you are experimenting with a Slack API that is still changing

That keeps the common path ergonomic without pretending the CLI fully models
every Slack API surface.
