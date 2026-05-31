# MCP Server Specifications: Chess Position Analyzer

## Purpose

Build a local MCP server that lets an AI client control a chess board and run Stockfish analysis for the current position. The server is for answering questions like:

- Why is this move good?
- Why is this move bad?
- What tactical or positional factors changed after this move?
- Which candidate moves are objectively stronger in this exact position?
- What does Stockfish expect to happen after this move?

The server is not an opening trainer. It must not query opening databases, user game histories, Lichess, Chess.com, ECO tables, repertoire files, or popularity statistics. It should analyze the position in front of it.

## High-Level Architecture

The MCP server runs locally and owns three pieces of state:

1. Board session (single, active)
   - current FEN
   - move history
   - optional labels/notes attached by the client
   - Only one session exists at a time. `create_board` replaces the previous session and frees its memory. Stale `session_id` values return `session_not_found`.

2. Stockfish process
   - one long-lived Stockfish child process, started lazily
   - configured Stockfish executable path
   - deterministic UCI options
   - per-request cancellation via UCI `stop`
   - engine health checks

3. Analysis cache
   - in-memory bounded LRU cache (default cap: 1024 entries, configurable)
   - key: normalized FEN (position + side to move + castling rights + en-passant square; halfmove clock and fullmove number are dropped) plus the analysis settings (`depth`, `multipv`)
   - the same position may hold multiple cached entries at different `depth`/`multipv` values
   - no external chess database cache
   - no opening-book cache

Implementation language: Rust.

Required crates:

- `rmcp` — official Rust MCP SDK; stdio transport
- `shakmaty` — chess move generation, FEN parsing, legality
- `tokio` — async runtime, Stockfish process I/O
- `serde` / `serde_json` — tool input/output schemas
- `lru` — bounded analysis cache
- `uuid` — session IDs
- `thiserror` — structured error model

Transport: stdio only (no HTTP/SSE in v1).

## Global Definitions

### Move Formats

Accept these move formats where applicable:

- `uci`: `e2e4`, `e7e8q`

Return UCI whenever the move is legal and unambiguous.

### Fixed Default Engine Settings

Default Stockfish options:

```json
{
  "Threads": 3,
  "Hash": 128,
  "UCI_AnalyseMode": true,
  "Contempt": 0
}
```

Default analysis constants:

```json
{
  "depth_main": 20,
}
```

Clients may request lower depths for faster interactive use, but every response must echo the effective settings.

## MCP Capabilities

### Tools

The server exposes tools for board control and analysis. Tool names use `snake_case`.

The complete tool list for v1 is exactly:

- `create_board`
- `get_board`
- `make_move`
- `undo_move`
- `engine_status`
- `analyze_position`

There is no `set_board` and no `redo_move` in v1.

### Resources

The server exposes read-only resources for active sessions and server status.

### Prompts

Prompts are optional. The first version can rely entirely on tools.

## Board Control Tools

### `create_board`

Create a board session.

Input:

```json
{
  "fen": "string | null",
  "moves_uci": ["string"],
}
```

Rules:

- If `fen` is omitted, start from the standard starting position.
- Apply `moves_uci` after the base FEN.
- Return a stable `session_id`.

Output:

Only one session is active at a time. When a new session is created via `create_board`, the previous session is destroyed and its memory freed. Any subsequent tool call using a stale `session_id` returns `session_not_found`.

```json
{
  "session_id": "uuid",
  "fen": "string",
  "turn": "white | black",
  "move_number": 1,
  "history": [],
  "board_text": "string",
  "legal_move_count": 20,
  "state": "normal | check | checkmate | stalemate",
  "legal_moves": ["e2e4", "d2d4"]
}
```

### `get_board`

Return the current state of a board session.

Input:

```json
{
  "session_id": "uuid"
}
```

Output:

The same as in `create_board`

### `make_move`

Apply one legal move to a board session.

Input:

```json
{
  "session_id": "uuid",
  "move": "string"
}
```

Rules:

- Use UCI
- Reject illegal moves with a structured error that includes legal alternatives when possible.

Output:

Same as `create_board`

### `undo_move`

Move backward in session history.

Input:

```json
{
  "session_id": "uuid",
  "plies": 1
}
```

Output: same shape as `get_board`.

## Stockfish Analysis Tools

### `engine_status`

Check Stockfish availability and configuration.

Input:

```json
{}
```

Output:

```json
{
  "available": true,
  "path": "stockfish/stockfish.exe",
  "name": "Stockfish",
  "uci_options": {
    "Threads": 3,
    "Hash": 128,
    "UCI_AnalyseMode": true,
    "Contempt": 0
  }
}
```

### `analyze_position`

Analyze the current position and return ranked candidate moves.

Input:

```json
{
  "session_id": "uuid | null",
  "fen": "string | null",
  "depth": 20,
  "multipv": 6
}
```

Rules:

- Exactly one of `session_id` or `fen` must be supplied. If both or neither are given, return a structured error.
- Analyze only the supplied FEN or current session position.
- Use Stockfish `go depth <depth>` with `MultiPV=<multipv>`.
- Return full principal variations in UCI (no PV length cap).
- Before searching, check the analysis cache. The cache key is the normalized FEN (position + side to move + castling rights + en-passant square) plus `depth` and `multipv`. On cache hit, return immediately without invoking Stockfish.
- On cache miss, run the search and store the result in the cache before returning.

Output:

```json
{
  "schema_version": "1.0",
  "fen": "string",
  "depth": 20,
  "multipv": 6,
  "lines": [
    {
      "rank": 1,
      "score": {
        "type": "cp",
        "cp": 31,
        "mate_in": null,
      },
      "pv_uci": ["e2e4", "e7e5"]
    }
  ]
}
```


## Error Model

All tool errors should be structured:

```json
{
  "error": {
    "code": "invalid_fen | illegal_move | engine_unavailable | engine_timeout | analysis_cancelled | session_not_found",
    "message": "Human-readable message.",
    "details": {}
  }
}
```

Examples:

- `invalid_fen`: include parser message and original FEN.
- `illegal_move`: include attempted move, format, current FEN, and legal moves if the list is reasonably small.
- `engine_unavailable`: include expected Stockfish path and setup hint.
- `engine_timeout`: include depth, elapsed time, and whether partial analysis is available.

## Progress and Cancellation

Long-running tools report progress via MCP progress notifications:

- `analyze_position`: emits `engine_started`, one `depth_complete` per completed iteration (carrying current best line), and `completed` when finished.

Cancellation follows the MCP cancellation flow. On cancel, the server sends UCI `stop` to Stockfish, drains output until `bestmove`, and returns the structured error `analysis_cancelled`. Cancelled searches are not written to the cache.

## Stockfish Process Requirements

Default paths (relative to the server's current working directory):

- Windows: `stockfish/stockfish.exe`
- macOS/Linux: `stockfish/stockfish`

The Stockfish executable path is resolved in this priority order (first match wins):

1. Command-line argument: `--stockfish-path <PATH>`
2. Environment variable: `CHESS_MCP_STOCKFISH_PATH`
3. Config file: `./chess-mcp.toml`, key `stockfish_path`
4. Platform default (above)

The same priority chain applies to other configurable settings (e.g. cache capacity, log level).

The server must:

- start Stockfish lazily
- send `uci`, wait for `uciok`
- send `isready`, wait for `readyok`
- configure deterministic UCI options
- reset `MultiPV` as needed per request
- shut down child processes on server exit
- never expose arbitrary process execution through MCP tools

## Security and Locality

The server is local-first.

It must not:

- call external chess APIs
- download games
- query opening databases
- expose filesystem reads outside configured Stockfish paths and optional log/cache directories
- accept arbitrary shell commands

It may:

- read the configured Stockfish executable
- write in-memory analysis cache
- optionally write local debug logs when enabled

## Testing Requirements

Unit tests:

- Cover everything with unit tests

Integration tests:

- start real Stockfish from default path
- create a board from FEN
- run `analyze_position` with `MultiPV=3`
- verify JSON serialization shape

If Stockfish is missing, integration tests should fail with a clear setup message or be explicitly marked as ignored by default.

## First Implementation Scope

The first implementation must include:

- MCP server startup over stdio using `rmcp`
- Stockfish status check (`engine_status`)
- Board session create/get (`create_board`, `get_board`) — single active session, replaced on create
- Make/undo move (`make_move`, `undo_move`)
- Legal move listing (returned as part of board state)
- Position analysis (`analyze_position`) with progress notifications and cancellation
- In-memory LRU analysis cache (default cap 1024)
- Stockfish path resolution via CLI arg > env var > config file > default
- Unit tests covering board logic, FEN normalization, cache behavior, UCI parsing, and error mapping
- One real Stockfish integration test (`#[ignore]` if Stockfish is missing, with a clear setup hint)