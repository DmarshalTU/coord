# Working with `coord`

You are running inside a multi-tab session where every tab — possibly
across different IDEs (Claude Code, Cursor, Codex, ...) — shares state
through `coord`, a local coordinator with an MCP server and a CLI.

This file is the protocol you should follow.

## How you talk to coord

You have two equivalent paths and should pick whichever your runtime
gives you:

- **MCP tools** (preferred when available): `tasks_send`, `tasks_list`,
  `tasks_get`, `tasks_claim`, `tasks_complete`, `tasks_cancel`,
  `agents_heartbeat`, `agents_list`. The MCP server is registered in
  `.mcp.json` at the project root.
- **Shell**: the `coord` binary on PATH. `coord send …`, `coord claim …`,
  `coord wait …`, `coord top`. Use this for `coord wait`, which blocks
  the calling process until a task appears — the cleanest way to make a
  tab a "watcher" without polling.

If both are present, use MCP tools for one-shot calls and `coord wait` in
shell for blocking watches.

## Pick a handle

On your first turn, pick a stable two-word handle (e.g. `cargo-otter`,
`harbor-lynx`, `ledger-fox`) and heartbeat as that ID. The handle should
hint at *what* you're doing in this tab so the operator can spot you in
`coord top`. Use the same ID for every subsequent call this session.

## On every turn, before doing anything else

1. Heartbeat (`agents_heartbeat`) so the operator sees you're alive.
2. Scan the bulletin board:

       tasks_list { state: "pending", priority: "high" }
       tasks_list { state: "pending", priority: "urgent" }

   If any pending bug or feature touches the file you're about to edit,
   *stop and address it first* — that's the whole point of the system.
   Another tab may have just found a regression in code you were about
   to ship.

## When you finish meaningful work

Post a `kind=ack` task with a name that's grep-friendly for the tabs
that might be waiting on you. Acks are completed-on-creation, so other
tabs see them via `coord wait --kind ack --name-contains '...'` without
needing to claim them.

```json
{
  "name": "v1.1 prod stable: discount math regression fixed",
  "kind": "ack",
  "priority": "high",
  "payload": {
    "fixed_bug_id": "276e7a7e-...",
    "sha": "e56f5fa",
    "branch": "fix/v1.1-discount-sign",
    "files": ["src/lib.rs"]
  }
}
```

If your ack relates to a specific bug task, put its UUID in
`fixed_bug_id` — `coord` writes a wikilink between the two notes in the
markdown vault, so Obsidian's graph view shows the chain.

## When you find a bug or have a question for another tab

Post `kind=bug` (or `feature`, `decision`, `knowledge`). Don't claim or
complete it — leave it `pending` so the right tab picks it up.

```json
{
  "name": "validate_token off-by-one at day boundary",
  "kind": "bug",
  "priority": "urgent",
  "payload": {
    "file": "src/auth.rs",
    "line": 42,
    "failing_test": "tests/auth_test.rs::token_expiring_exactly_now_is_still_valid",
    "repro": "cargo test --test auth_test"
  }
}
```

## When you claim work

Use `tasks_claim` *atomically*. If it returns "task is not claimable"
that's the expected race signal — somebody else got it; pick another.
After you finish, `tasks_complete` with a JSON `result`. If you can't
finish, do not silently abandon: post an ack-or-knowledge note
explaining what's left.

## When you're a watcher

Use `coord wait` from a shell call instead of polling in a loop. It
blocks until a matching task appears, heartbeats while waiting, and
prints the matched task as JSON.

```bash
coord wait --as harbor-lynx --name 'harbor lynx' \
  --kind ack --name-contains 'v1.1 prod stable' --timeout 600
```

State filtering is kind-aware: `--kind ack` defaults to
`--state completed` because acks are completed-on-creation; bug/feature
waits default to `--state pending`. Pass `--state any` to opt out.

## Don't

- Don't fabricate UUIDs to satisfy schema fields. If you don't have a
  real `fixed_bug_id`, omit it.
- Don't busy-loop with `tasks_list` when `coord wait` is available.
- Don't claim and abandon — either complete, or post a note explaining
  the abandonment and let the operator/another tab pick it up.

## Recap before you stop

End each turn with a one-paragraph recap: what you did, what's now in
the ledger (bug IDs, ack IDs, commit SHAs), and what the next tab
should do. The vault keeps the long-form history; the recap is for the
operator scanning the chat.
