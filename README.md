# coord

**A local coordinator for parallel AI coding agents. One binary, MCP + A2A.**

You run multiple Claude Code / Cursor / Codex tabs in parallel and they have no
idea the others exist. The tab on `v1.1` finds a regression. The tab on `v1.2`
keeps building on top of it because nobody told it. `coord` gives those agents
a shared bulletin board with **atomic task claims** (no two agents grab the
same work), a **blocking watch primitive** (so an agent can wait on something
landing instead of polling), and an optional **markdown audit trail** that
opens in Obsidian as a graph.

```
   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
   │ Claude Code  │  │   Cursor     │  │   Codex      │   ...N apps
   └──────┬───────┘  └──────┬───────┘  └──────┬───────┘
          │  MCP            │  MCP            │  MCP
   ┌──────▼─────────────────▼─────────────────▼───────┐
   │              coord serve  (HTTP A2A)             │
   │   atomic claims · heartbeats · vault · TUI       │
   └──────────────────────────────────────────────────┘
                  SQLite (WAL) · markdown vault
```

## Status

POC. Built in a weekend, used in real demos, two end-to-end tests prove the
correctness guarantees, but the surface area is still small. `0.x` until it
gets meaningful production use.

## Why another one of these?

There are several local-coordination layers for AI agents shipping in 2026
([prior art](#prior-art)). `coord` is opinionated about a few specific things:

- **Atomic claims, not mailboxes.** Most existing tools are messaging /
  pub-sub layers. `coord` exposes a race-free `tasks/claim` so two agents can
  both grab for the same task and exactly one wins. The
  [`tests/race.rs`](tests/race.rs) test hammers 200 tasks × 8 claimers each
  (1,600 simultaneous claim attempts) and asserts every task ends up with
  exactly one winner. The
  [`tests/multi_client.rs`](tests/multi_client.rs) test does the same end-to-
  end, with 16 independent OS processes racing over HTTP.
- **Blocking watch primitive.** `coord wait --kind ack --name-contains 'v1.2'`
  blocks the calling shell until a matching task lands and prints it as JSON.
  It heartbeats while waiting so the watcher shows as alive. This is what
  turns a Claude Code tab into a "waiter" with a single chat message instead
  of an instruction loop.
- **Two protocols, one daemon.** Speaks A2A (Google's agent-to-agent JSON-RPC
  subset) on its HTTP surface and acts as an MCP server over stdio for IDE
  clients that don't speak A2A directly.
- **Optional Obsidian-readable vault.** Every state change emits a markdown
  note with `[[wikilinks]]` between related tasks (bug → fix → ack). Drop the
  vault into Obsidian; the graph view shows who-did-what across all sessions
  with no plugin.
- **One binary.** `coord serve`, `coord top`, `coord send`, `coord wait`,
  `coord mcp` — all the same executable. No Python venv, no Docker.

If you want a richer mailbox/email metaphor with file leases and threading,
[MCP Agent Mail](https://github.com/Dicklesworthstone/mcp_agent_mail) is the
mature choice. `coord` is the one to reach for if you want claim-and-watch
semantics with a tiny surface area.

## Install

### Homebrew

```bash
brew tap dmarshaltu/coord
brew install coord
```

### From source

```bash
cargo install --git https://github.com/DmarshalTU/coord
```

### Pre-built binaries

Tagged releases attach binaries for macOS (Apple Silicon), Linux
(x86\_64), and Windows (x86\_64). On Intel Macs and ARM Linux, build from
source with `cargo install --git ...` instead.

```bash
# example: macOS Apple Silicon
curl -L https://github.com/DmarshalTU/coord/releases/latest/download/coord-aarch64-apple-darwin -o /usr/local/bin/coord
chmod +x /usr/local/bin/coord
```

## Quickstart

```bash
# 1. start the daemon (long-lived; run it once and forget it)
coord serve --vault ~/coord-vault

# 2. in your project, scaffold .mcp.json + AGENTS.md
cd ~/code/my-project
coord init

# 3. open Claude Code / Cursor / Codex in the project — coord shows up as an MCP server

# 4. watch it live
coord top
```

`coord init` drops two files:

- **`.mcp.json`** — Claude-Code-style MCP config pointing at `coord mcp`.
  Claude Code picks it up automatically. Cursor, Codex, and Gemini CLI
  accept it with one extra step (see [Setup per IDE](#setup-per-ide)).
- **`AGENTS.md`** — the protocol every agent in this project should follow
  (handles, heartbeats, scanning the bulletin, posting acks, using
  `coord wait`). Pasted as `CLAUDE.md` / `.cursorrules` / system prompt for
  IDEs that look elsewhere — see below.

If you'd rather do it by hand, the Quickstart above is just:

```json
{
  "mcpServers": {
    "coord": { "command": "coord", "args": ["mcp"] }
  }
}
```

## Setup per IDE

Anything that speaks MCP can drive `coord` (`tasks_send`, `tasks_claim`,
`tasks_complete`, `agents_heartbeat`, `tasks_list`, `tasks_get`,
`agents_list`). The protocol is the same; only where you put the config
differs.

### Claude Code

`coord init` is enough. Claude Code reads `.mcp.json` from the project root
on next launch. Confirm with `claude mcp list` — you should see `coord`.
Drop `AGENTS.md` (which `coord init` writes for you) at the project root and
Claude Code reads it as the agent protocol.

You can also register `coord` globally instead of per project:

```bash
claude mcp add coord -- coord mcp
```

### Cursor

After `coord init`, copy the same config into `.cursor/mcp.json` (Cursor
reads MCP servers from there, not from `.mcp.json`):

```bash
mkdir -p .cursor && cp .mcp.json .cursor/mcp.json
```

Cursor also reads project rules from `.cursorrules` rather than `AGENTS.md`,
so symlink:

```bash
ln -sf AGENTS.md .cursorrules
```

### Codex CLI

Codex configures MCP servers in `~/.codex/config.toml`:

```toml
[mcp_servers.coord]
command = "coord"
args = ["mcp"]
```

Codex reads `AGENTS.md` from the project root natively, so the file
`coord init` writes Just Works.

### Gemini CLI / other MCP clients

Most MCP clients accept the same JSON shape. Point them at:

```json
{ "command": "coord", "args": ["mcp"] }
```

…and paste `AGENTS.md` into whatever the client uses for system context.

### Watch what's happening

```bash
coord top
```

```
┌coord─────────────────────────────────────────────────────────────────┐
│coord top  •  2 active  0 idle  0 stale  (2 agents total)             │
│tasks: 3 visible / 3 total  pending=1 claimed=1 completed=1           │
│filter: active  •  detail: on  •  refreshed 0.2s ago                  │
└──────────────────────────────────────────────────────────────────────┘
┌agents (2)──────────────┐┌tasks (3/3)──────────────────────────┐
│   ID            UPTIME ││  ID    AGE   PRIO    KIND   STATE   │
│●  feature-a-v1.2 1m23s ││▶ 14f5  27s   normal  bug    pending │
│●  hotfix-v1.1    2m05s ││  5ed1  56s ▲ high    ack    completed│
│                        ││  6ab8  1m09s normal  knowl  completed│
└────────────────────────┘└─────────────────────────────────────┘
```

## Two-prompt demo

Two Claude Code tabs, two release branches, no operator coordination:

**Tab A (v1.2 release prep):**
> Prep the v1.2 release. Wait for v1.1 to be stable before shipping the build.

**Tab B (v1.1 hotfix):**
> Test the v1.1 hotfix branch. If it's red, fix it and post a stable ack.

Tab B runs the tests, finds a regression, fixes it, commits, posts a
`kind=ack` task. Tab A's `coord wait --kind ack --name-contains 'v1.1 stable'`
unblocks the instant that ack lands and ships the v1.2 build artifact. Two
prompts, zero coordination from the operator.

A full reproducible scenario lives in
[`scripts/demo.sh`](scripts/demo.sh).

## Architecture

```
src/
├── main.rs              entry, parses CLI
├── server.rs            serve / mcp / version
├── lib.rs               library re-exports
├── cli/                 client subcommands
│   ├── mod.rs           dispatch
│   ├── client.rs        blocking JSON-RPC client
│   ├── format.rs        plain-text printers
│   ├── tui.rs           ratatui dashboard
│   └── wait.rs          blocking watch primitive
├── core/
│   ├── store.rs         SQLite + atomic claim
│   └── types.rs         wire types
├── a2a/mod.rs           HTTP JSON-RPC server (axum)
├── mcp/mod.rs           stdio MCP bridge (rmcp)
└── vault/mod.rs         markdown audit trail
```

### Concurrency model

- One `coord serve` process per project. Multiple agents and IDEs all connect
  over loopback HTTP; `tokio` + `axum` handle the concurrent connections.
- Storage: SQLite in WAL mode. A single connection guarded by a `Mutex`
  serialises writes (correct for SQLite); concurrent readers are unaffected.
  At the workloads `coord` is built for (a few dozen agents, a few thousand
  tasks/day) this is comfortable.
- Atomic claims: `UPDATE tasks SET state='claimed' WHERE id=? AND
  state='pending'` returning rowcount. The DB enforces single-winner.

### Task kinds and states

Tasks have a free-form `kind` and `priority` plus a fixed lifecycle:

```
pending  ──claim──▶  claimed  ──complete──▶  completed
                            └──cancel────▶   cancelled
                            └──fail──────▶   failed
```

There's a useful asymmetry: announcement kinds (`ack`, `knowledge`,
`decision`) start in `completed` rather than `pending` — they're publications,
not work to be picked up. The TUI's default filter still surfaces them as
context.

### A2A subset

`coord` implements a small JSON-RPC 2.0 subset of Google's A2A spec on
`POST /` plus an agent card at `GET /.well-known/agent.json`. Methods:

| Method            | Purpose                                |
|-------------------|----------------------------------------|
| `tasks/send`      | create a task                          |
| `tasks/get`       | fetch a task by UUID                   |
| `tasks/list`      | list recent tasks (with kind/state filters) |
| `tasks/cancel`    | cancel a task                          |
| `tasks/claim`     | atomic pending→claimed (extension)     |
| `tasks/complete`  | claimed→completed with result (extension) |
| `agents/heartbeat`| register/refresh agent presence        |
| `agents/list`     | list known agents                      |

### MCP tools

The same operations are exposed as MCP tools (see
[`src/mcp/mod.rs`](src/mcp/mod.rs)) so any MCP-aware client can drive `coord`
without a custom integration.

## CLI

```
coord serve          run the daemon (HTTP A2A surface)
coord mcp            stdio MCP bridge for IDE clients
coord init           scaffold a project (.mcp.json + AGENTS.md)
coord top            live TUI dashboard
coord status         one-shot summary
coord tasks          list recent tasks
coord agents         list known agents
coord send <name>    create a task (--kind, --priority, --payload)
coord claim <id>     atomic claim (--as <agent>)
coord complete <id>  mark a claimed task complete (--result <json>)
coord cancel <id>    cancel a task
coord heartbeat <id> refresh agent presence
coord wait           block until a matching task appears
coord version        version + protocol info
```

Every client subcommand obeys `--url` / `COORD_URL` (default
`http://127.0.0.1:7777/`).

## Configuration

| Flag / env var          | Default                                     |
|-------------------------|---------------------------------------------|
| `--addr` / `COORD_ADDR` | `127.0.0.1:7777`                            |
| `--db` / `COORD_DB`     | per-user data dir (`directories` crate)     |
| `--vault` / `COORD_VAULT` | unset (markdown vault disabled)            |
| `--url` / `COORD_URL`   | `http://127.0.0.1:7777/`                    |

## Tests

```
cargo test
```

Three end-to-end tests gate every change:

- `tests/race.rs` — atomic claim under in-process contention
- `tests/multi_client.rs` — atomic claim under multi-process contention
  (real `coord serve` + 16 OS processes over HTTP)
- `tests/cancel_race.rs` — cancel-vs-complete is sticky
- `tests/tui_render.rs` — TUI snapshot test

## Prior art

Several local-coordination layers for AI agents exist as of 2026 and you
should know about them before picking one:

- [**MCP Agent Mail**](https://github.com/Dicklesworthstone/mcp_agent_mail) —
  the most mature option in the space. Mailbox/email model with agent
  identities, advisory file reservations, threaded archives, FastMCP server,
  Git + SQLite backing store.
- [**SynapBus**](https://github.com/synapbus/synapbus) — Go, single binary,
  MCP-native messaging with a Slack-like web UI and semantic search.
- [**Agent Tower**](https://github.com/dbendaou/mcp-agent-tower) — local HTTP
  daemon with resource locking, announcements, and issue tracking.
- [**cross-agent-teams-mcp**](https://jtianling.com/en/cross-agent-teams-release.html)
  — minimal SQLite-backed message bus with a wake channel.

`coord` differs in shape, not in problem space — claim-and-watch semantics, a
small JSON-RPC + MCP surface, and an Obsidian-readable vault are the
deliberate trade-offs. If your workflow leans on threaded messaging or file
leases, MCP Agent Mail is probably the better fit.

Anthropic ships an experimental
[Agent Teams](https://code.claude.com/docs/en/agent-teams) feature in Claude
Code itself; it's lead/teammate orchestration, not a shared bulletin board, so
it's complementary rather than competitive.

## License

MIT. See [`LICENSE`](LICENSE).
