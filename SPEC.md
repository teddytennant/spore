# spore design

## Thesis

Most agent harnesses grow a tool per capability: read, write, edit, search, run,
browse, delegate. spore has one tool, `bash`, and gets the rest from the shell —
already the universal interface to a computer, and the agent sits inside one.

Three things that usually ship as "advanced harness features" fall out of that
single tool with no extra code:

1. **Files, builds, tests, git, network.** Plain shell commands.
2. **Subagents.** spore is a program on `PATH`, so it can run `spore -p "task"`
   from bash. Each call is a fresh headless instance that prints its result to
   stdout, capturable with `$(…)` and parallelizable with `&`/`wait`. A depth
   guard (`SPORE_DEPTH`) bounds recursion so it cannot fork-bomb.
3. **Self-extension.** spore knows its own source tree (`$SPORE_HOME`, baked in
   at build time), so it can edit `src/main.rs` or `prompt.md`, rebuild, and
   reinstall — genuinely modifying the harness it runs on.

## Shape

```
prompt.md ──include_str!──▶ system prompt
                                │
   user ──▶ Chat Completions (streaming SSE) ──▶ text + tool_calls(bash)
                                │                     │
                                │              std::process bash -c
                                │                     │
                                ◀──── role:tool ──────┘   (loop until no calls)
```

- **Wire format:** OpenAI-compatible Chat Completions only, streamed over SSE.
  Supported setups: xAI Grok with an API key (default), xAI Grok with OAuth
  (`SPORE_API_KEY_CMD`), and an OpenAI endpoint via `SPORE_BASE_URL`.
- **Loop:** call the model; run every bash call it asks for; feed the output back
  as `role: tool` messages; repeat until it asks for none.
- **Auth:** a static key, or `SPORE_API_KEY_CMD` — a command run fresh per
  request, so a refreshing OAuth token never goes stale.
- **Modes:** interactive line REPL, or headless (`-p`) for one-shot and subagent
  use. Headless streams progress to stderr and prints only the final answer to
  stdout, so a parent captures a clean result.
- **Dependencies:** `ureq` and `serde_json`. No async runtime, no readline
  library, no TUI, no spinner.

## Configuration

| Env | Default | Purpose |
|---|---|---|
| `XAI_API_KEY` / `OPENAI_API_KEY` / `SPORE_API_KEY` | (none) | static auth key |
| `SPORE_API_KEY_CMD` | (none) | command printing a fresh key each request (OAuth) |
| `SPORE_MODEL` | `grok-4.5` | model id |
| `SPORE_BASE_URL` | `api.x.ai/v1/chat/completions` | OpenAI-compatible endpoint |
| `SPORE_HOME` | crate dir (build-time) | source tree for self-extension; set it explicitly when installed via `cargo install --git` (the build-time dir is a deleted temp checkout) |
| `SPORE_DEPTH` | `0` | subagent recursion depth (internal) |

## Non-goals

Sandboxing, an approval UI, a plugin system, a full-screen TUI. Each is omitted
on purpose: the point is the smallest harness that still does real work. Anything
missing, spore can add to itself.
