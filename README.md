# spore

A minimal coding agent with one tool: **bash**. No approval gate. It edits code,
runs builds and tests, uses git and curl, spawns subagents (`spore -p "task"`),
and can rewrite and recompile its own source. ~700 lines of Rust, two
dependencies.

## Install

```sh
cargo install --git https://github.com/teddytennant/spore
```

Or download the prebuilt static binary (x86-64 Linux, musl):

```sh
curl -fsSL https://github.com/teddytennant/spore/releases/latest/download/spore \
  -o ~/.local/bin/spore && chmod +x ~/.local/bin/spore
```

Each release ships a `spore.sha256` checksum next to the binary. The prebuilt
binary has no source tree baked in, so for self-extension set `SPORE_HOME` to
a checkout of this repo.

## Use

```sh
spore                                      # interactive session
spore -p "fix the failing test in ./src"   # one-shot; prints result and exits
spore login                                # provider setup wizard
```

Started in a terminal with no credentials configured, spore drops into the
setup wizard automatically. Inside the REPL, `/login` reruns it and `/exit`
quits.

## Providers

spore speaks one wire format, the OpenAI-compatible Chat Completions API. The
wizard knows eight providers:

| # | Provider | Model | Base URL |
|---|----------|-------|----------|
| 1 | xai | `grok-4.5` | `https://api.x.ai/v1/chat/completions` |
| 2 | openai | `gpt-5.2` | `https://api.openai.com/v1/chat/completions` |
| 3 | anthropic | `claude-sonnet-5` | `https://api.anthropic.com/v1/chat/completions` |
| 4 | gemini | `gemini-2.5-pro` | `https://generativelanguage.googleapis.com/v1beta/openai/chat/completions` |
| 5 | groq | `moonshotai/kimi-k2-instruct` | `https://api.groq.com/openai/v1/chat/completions` |
| 6 | deepseek | `deepseek-chat` | `https://api.deepseek.com/v1/chat/completions` |
| 7 | openrouter | `openrouter/auto` | `https://openrouter.ai/api/v1/chat/completions` |
| 8 | ollama | `qwen3` | `http://localhost:11434/v1/chat/completions` (local, no key) |

For hosted providers the wizard opens the key console in your browser, reads
the pasted key with echo off, and writes `~/.config/spore/config` (mode
`0600`).

### Sign in with xAI (no API key needed)

Picking **xai** in the wizard offers **Sign in with browser**: an OAuth 2.0
device flow against `auth.x.ai`. It prints a short code, opens the approval
page, and polls until you click Allow. **A Grok / X Premium subscription works
here — no separate API key or billing required.** Tokens land in
`~/.config/spore/oauth` (mode `0600`); the 6-hour access token refreshes
automatically and the rotating refresh token is re-persisted on every refresh.

### Configuration

Environment variables always win, then `~/.config/spore/config` (same names,
`KEY=value` lines), then compiled defaults. A static key beats OAuth, and
`SPORE_API_KEY_CMD` beats both.

```sh
export XAI_API_KEY=xai-...                 # or OPENAI_API_KEY / SPORE_API_KEY

SPORE_API_KEY_CMD="my-oauth --print-token" spore   # fresh token per request

SPORE_BASE_URL=https://api.openai.com/v1/chat/completions \
  SPORE_MODEL=gpt-5.2 OPENAI_API_KEY=sk-... spore
```

## Warning

spore runs shell commands immediately, unsandboxed, with no confirmation. Use it
only somewhere you are willing to let it touch.

See [SPEC.md](SPEC.md) for the design.
