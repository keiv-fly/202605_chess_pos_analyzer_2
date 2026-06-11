# Chess Position Analyzer

Rust workspace with two binaries:

- `chess-pos-analyzer`: local MCP server exposing board and Stockfish tools.
- `chess-agent`: OpenRouter-powered chat agent that uses the MCP server.

## Setup

Copy `.env.example` to `.env` and set an OpenRouter key:

```powershell
Copy-Item .env.example .env
```

Then edit `.env`:

```text
OPENROUTER_API_KEY=sk-or-v1-...
OPENROUTER_MODEL=openrouter/auto
```

Stockfish is expected to be available through the server configuration or the repo-local `stockfish` folder.

## Dialog With The Agent

Start a continuing dialog:

```powershell
cargo run --package chess-agent -- --chat
```

You can also seed the dialog with an initial question:

```powershell
cargo run --package chess-agent -- --chat "Analyze this FEN: rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
```

Inside dialog mode:

- `/help` shows commands.
- `/clear` clears conversation context.
- `/exit` quits.

For one-shot use, pass a prompt without `--chat`:

```powershell
cargo run --package chess-agent -- "Analyze the starting position at depth 12"
```
