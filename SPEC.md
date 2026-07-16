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
  Eight provider presets (xai, openai, anthropic, gemini, groq, deepseek,
  openrouter, ollama) all speak it; any other endpoint works via
  `SPORE_BASE_URL`.
- **Loop:** call the model; run every bash call it asks for; feed the output back
  as `role: tool` messages; repeat until it asks for none.
- **Auth:** a static key, `SPORE_API_KEY_CMD` (a command run fresh per request),
  or a built-in xAI OAuth sign-in (see Configuration).
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

Environment variables win, then `~/.config/spore/config` (same names as
`KEY=value` lines), then compiled defaults. For auth, `SPORE_API_KEY_CMD`
beats a static key, which beats OAuth.

**Onboarding:** with no credentials configured, an interactive run drops into a
setup wizard (also reachable via `spore login` or `/login` in the REPL): pick
one of eight OpenAI-compatible providers, and either paste an API key (echo
off, console page opened via `xdg-open`) or — for xAI — sign in with a Grok /
X Premium subscription. The xAI path is a standard RFC 8628 device flow
against `auth.x.ai` using the public grok-cli client: request a device code,
open the verification URL in the browser, poll the token endpoint until
approval. Tokens land in `~/.config/spore/oauth` (mode 0600); the access token
(6 h) is auto-refreshed with a 120 s skew, and the rotating refresh token is
re-persisted on every refresh. The resulting bearer is used against the normal
chat-completions endpoint.

## Non-goals

Sandboxing, an approval UI, a plugin system, a full-screen TUI. Each is omitted
on purpose: the point is the smallest harness that still does real work. Anything
missing, spore can add to itself.
