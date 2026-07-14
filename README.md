# spore

A minimal coding agent with one tool: **bash**. No approval gate. It edits code,
runs builds and tests, uses git and curl, spawns subagents (`spore -p "task"`),
and can rewrite and recompile its own source. ~270 lines of Rust, two
dependencies.

## Install

```sh
cargo install --git https://github.com/teddytennant/spore
```

## Use

```sh
export XAI_API_KEY=xai-...
spore                                      # interactive session
spore -p "fix the failing test in ./src"   # one-shot; prints result and exits
```

## Providers

spore speaks one wire format, the OpenAI-compatible Chat Completions API:

- **xAI Grok, API key** (default): `XAI_API_KEY=xai-...`, model `grok-4.5`.
- **xAI Grok, OAuth**: set `SPORE_API_KEY_CMD` to a command that prints a fresh
  token per request, so a refreshing token never goes stale.
- **OpenAI**: point `SPORE_BASE_URL` at it.

```sh
SPORE_API_KEY_CMD="my-xai-oauth --print-token" spore

SPORE_BASE_URL=https://api.openai.com/v1/chat/completions \
  SPORE_MODEL=gpt-5.1 OPENAI_API_KEY=sk-... spore
```

## Warning

spore runs shell commands immediately, unsandboxed, with no confirmation. Use it
only somewhere you are willing to let it touch.

See [SPEC.md](SPEC.md) for the design.
