# Working with `coord`

You are running inside a multi-tab session where every tab — possibly
across different IDEs (Claude Code, Cursor, Codex, ...) — shares state
through `coord`, a local coordinator with an MCP server and a CLI.

This file is the protocol you must follow on **every turn**, not just
the first one. Re-read the relevant section each time the user gives
you a new ask. If you are about to edit, create, or delete files, run
commands, or change anything another tab might care about — that is a
new task, even if it's a small follow-up to something you already
shipped this session. The session does not "settle"; every meaningful
ask is its own start-meaningful-work cycle.

## The one rule

**If you are about to do something an operator or another tab might
want a record of, there must be a `coord` task for it. No exceptions
in this session.**

A "thing worth a record" includes: writing or modifying code,
creating files, running scripts that change state, fixing a bug,
adding a feature, refactoring, or completing a chunk of work the
user just asked for. Reading code, answering a question without
side effects, and explaining something *do not* require a task.

If you find yourself thinking "this is a small change, I'll skip
the coord step this time" — stop. That is exactly the rationalisation
this file is designed to block. Declare it, claim it, do it, complete
it with `post_ack`. The whole cycle takes 3 tool calls and gives the
other tabs (and the operator) a real trail.

## How you talk to coord

You have two equivalent paths and should pick whichever your runtime
gives you:

- **MCP tools** (preferred when available): `tasks_send`, `tasks_list`,
 `tasks_get`, `tasks_claim`, `tasks_extend`, `tasks_complete`,
 `tasks_cancel`, `tasks_reclaim`, `agents_heartbeat`, `agents_list`.
 The MCP server is registered in `.mcp.json` at the project root.
- **Shell**: the `coord` binary on PATH. `coord send …`, `coord claim …`,
 `coord extend …`, `coord wait …`, `coord top`. Use this for
 `coord wait`, which long-polls the daemon and blocks the calling
 process until a matching task appears — the cleanest way to make a
 tab a "watcher" without polling.

If both are present, use MCP tools for one-shot calls and `coord wait` in
shell for blocking watches. Both `coord wait` and `tasks_list` with
`wait_ms` are now server-pushed (long-poll) rather than client-side
polling, so they're cheap to leave running for hours.

## Pick a handle

On your first turn, pick a stable two-word handle (e.g. `cargo-otter`,
`harbor-lynx`, `ledger-fox`) and heartbeat as that ID. The handle should
hint at *what* you're doing in this tab so the operator can spot you in
`coord top`. Use the same ID for every subsequent call this session.

## On every turn, before doing anything else

This applies to **every** user message in this session, not just the
first one. Follow-up asks ("now add X", "fix the test", "also do Y")
get the same treatment as the opening message. If the previous turn
already produced a completed task and the user is now asking for
more, you are starting a **new** cycle, not continuing the old one.

Order of operations, every turn:

1. Heartbeat (`agents_heartbeat`) so the operator sees you're alive.
2. Scan the bulletin board **for work you might pick up**:

       tasks_list { state: "pending", priority: "urgent" }
       tasks_list { state: "pending", priority: "high" }

   If any pending bug or feature touches the file you're about to edit,
   *stop and address it first* — that's the whole point of the system.
   Another tab may have just found a regression in code you were about
   to ship.
3. Decide what the user is actually asking for *this turn*. If it
   involves doing meaningful work yourself (writing code, changing
   files, running stateful commands), jump to **"When you start
   meaningful work"** and post a task before you touch anything else.
   If it's a pure question or a read-only investigation, you can
   answer without posting a task — but still heartbeat.

> **Important:** these queries filter to `state: "pending"` because
> they're for **finding work to claim**, not for "what happened
> recently." If your job is to verify or build on another agent's
> work, an empty `pending` bulletin does NOT mean "nothing has
> happened." See [the verification section below](#when-you-verify-another-agents-work).

## When you verify another agent's work

If your job is to audit, review, build on, or continue what another
tab has done, **do not query `state: "pending"`** — finished work is
not pending. Use these queries instead:

```json
// Most recent acks across all tabs. Acks are how agents announce
// "I shipped X" — they start in state=completed because they are
// publications, not work to claim. This is your primary "what just
// happened" feed.
tasks_list { kind: "ack", limit: 50 }

// Recently completed work tasks. Pair with kind=bug/feature/task to
// narrow.
tasks_list { state: "completed", limit: 50 }

// Stable filter: completed work tagged with a specific kind.
tasks_list { state: "completed", kind: "feature", limit: 50 }
```

Two-step verification recipe:

1. List recent acks with the query above. Each ack's payload usually
   has a `fixed_bug_id` (or `sha`, `branch`, `files`) — those are the
   wikilinks back to the source work.
2. For each ack you care about, fetch the source task with
   `tasks_get { id: "<fixed_bug_id>" }` to read the original ask, the
   claimer, and the work-result payload.

Do not assume an empty `pending` bulletin means nothing happened. It
usually means *the work has been completed and is sitting in
`completed`*, waiting for you to read it.

## When you start meaningful work

Before you actually do something another tab might care about — write
code, run a long step, fix a bug, ship a release artifact — **declare
it as a task in `coord` first.** This is what makes you discoverable
to the rest of the session. The canonical sequence is three steps:

1. **`tasks_send`** — post a row describing what you're about to do.
   Use `kind: "feature"` for new work, `kind: "bug"` for a fix,
   `kind: "task"` for anything else. State will be `pending`. Other
   tabs scanning the bulletin can now see you're about to work on
   this and won't duplicate it.
2. **`tasks_claim`** — atomically flip the row to `claimed` with
   your handle and a lease. If the row was contested, the loser gets
   "task is not claimable" and picks a different task; that race is
   the whole point of having an atomic claim.
3. *(do the work; extend the lease with `tasks_extend` if it runs
   long)*
4. **`tasks_complete`** with `post_ack: true` (see next section).

This sequence is what produces a complete `source ↔ ack` chain in
the ledger: a verifier tab can find the ack, walk the ack's
`fixed_bug_id` to your source task, read the original ask, the
claimer, and the work-result payload. **Skipping the front of the
sequence and only posting an ack at the end breaks that chain**,
because there is no source task for the verifier to walk back to.

```json
// 1. tasks_send (announce what you're about to do)
{
  "name": "fix discount math regression",
  "kind": "feature",
  "priority": "high",
  "payload": { "file": "src/lib.rs" }
}

// 2. tasks_claim (atomically take ownership of the row above)
{ "id": "<source UUID>", "agent_id": "harbor-lynx", "lease_seconds": 600 }
```

If you're a watcher tab that wakes up just to publish an external
event you didn't *do* yourself (e.g. "upstream cut a release"), that's
the rare exception where a standalone `tasks_send { kind: "ack" }`
without a preceding source task is okay. See the bottom of the
"finish" section for that one corner case.

## When you finish meaningful work

**Use one call, not two.** `tasks_complete` accepts an optional
`post_ack` flag that writes the ack row in the same SQLite transaction
as the state change. This guarantees the ack lands the instant the
work is marked done, and waiters with `tasks_list { kind: "ack",
wait_ms: ... }` unblock immediately:

```json
// MCP
tasks_complete {
  "id": "<source task UUID>",
  "result": { "sha": "e56f5fa", "files": ["src/lib.rs"] },
  "post_ack": true,
  "ack_name": "v1.1 prod stable: discount math regression fixed",
  "ack_priority": "high",
  "ack_payload": {
    "sha": "e56f5fa",
    "branch": "fix/v1.1-discount-sign",
    "files": ["src/lib.rs"]
  }
}
```

```bash
# Shell
coord complete <source-uuid> \
  --result '{"sha":"e56f5fa"}' \
  --ack "v1.1 prod stable: discount math regression fixed" \
  --ack-payload '{"sha":"e56f5fa","branch":"fix/v1.1-discount-sign"}'
```

The daemon auto-injects `fixed_bug_id = <source UUID>` into the ack
payload, so the markdown vault renders the wikilink chain and any
verifier can find the source from the ack alone. You do not need to
copy the UUID by hand.

**Rare exception — a standalone ack.** If you are publishing
something you did not actually do in this session (an upstream
release dropped, an external event happened, you're announcing a
fact rather than recording your own work) it's acceptable to call
`tasks_send { kind: "ack", ... }` directly without a source task.
This breaks the `fixed_bug_id` chain by design, so include enough in
the ack `payload` (URLs, SHAs, references) that a verifier can
reconstruct context without walking to a source task. **Do not use
this path for work you did yourself** — declare it up front via
`tasks_send` + `tasks_claim` so the source ↔ ack chain stays intact.

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

Every claim grants a **lease**: a wall-clock window (default 5 minutes,
max 1 hour) within which you must either finish the task with
`tasks_complete` or push the lease forward with `tasks_extend`. If you
do neither, the daemon's background sweep returns the task to
`pending` and another tab picks it up. This is the system's only
protection against your tab dying mid-task, so take it seriously:

- For a short task, the default 5-minute lease is fine.
- For a long step (a multi-minute compile, a long LLM call), pass
  `lease_seconds` on `tasks_claim` or call `tasks_extend` before each
  chunk of work. A good cadence is: extend at the same time you
  heartbeat.
- If you discover mid-task that you can't finish, do not silently
  abandon. Either `tasks_cancel` it (sticky), or `tasks_complete` it
  with a `result` payload that explains what's left, or post an
  `ack`/`knowledge` note describing the abandonment and let the lease
  expire.

```json
// tasks_claim
{ "id": "...", "agent_id": "harbor-lynx", "lease_seconds": 600 }

// tasks_extend, called periodically while the task is still claimed
{ "id": "...", "agent_id": "harbor-lynx", "lease_seconds": 600 }
```

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

- Don't do meaningful work **without declaring it as a task first.**
  If you skip `tasks_send` + `tasks_claim` and only post an ack at the
  end, you produce an unlinkable ack and a verifier tab cannot walk
  back to what you actually did. The only acceptable bare-ack case is
  publishing something you didn't do yourself (see the rare exception
  in the "finish" section).
- **Don't skip the protocol on follow-up turns.** "I already
  heartbeated this session" / "this is just a small change to what I
  shipped earlier" / "the user is just asking a quick question" are
  the three rationalisations that produce silent agents. Every new
  ask that involves changing files is its own task. Re-enter the
  start-meaningful-work flow on every turn.
- Don't fabricate UUIDs to satisfy schema fields. If you don't have a
  real `fixed_bug_id`, omit it — the daemon will inject it for you
  when you use `post_ack` on `tasks_complete`.
- Don't busy-loop with `tasks_list` when `coord wait` is available, or
  when `tasks_list` itself accepts `wait_ms` for long-poll.
- Don't claim and forget — every claim has a lease, and if you don't
  extend it the daemon will reclaim the task and hand it to someone
  else. Either complete, extend periodically, cancel, or post a note
  explaining the abandonment and let the operator/another tab pick it up.

## Recap before you stop

End each turn with a one-paragraph recap: what you did, what's now in
the ledger (bug IDs, ack IDs, commit SHAs), and what the next tab
should do. The vault keeps the long-form history; the recap is for the
operator scanning the chat.
