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

1. Board sessions
   - current FEN
   - move history
   - redo history
   - optional labels/notes attached by the client

2. Stockfish process pool
   - configured Stockfish executable path
   - deterministic UCI options
   - per-request cancellation
   - engine health checks

3. Analysis cache
   - in-memory cache keyed by normalized FEN plus analysis settings
   - no external chess database cache
   - no opening-book cache

Recommended implementation language: Rust with `shakmaty`.

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

Here and later in the text there is only one active session. When a new session is created then old session is not available anymore (memory freed)

```json
{
  "session_id": "uuid",
  "fen": "string",
  "turn": "white | black",
  "move_number": 1,
  "history": [],
  "board_text": "string",
  "legal_move_count": 20,
  "state": "normal | check | checkmate | stalemate"
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
    "Threads": 1,
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
  "multipv": 6,
  "max_plies": 12,
}
```

Rules:

- Analyze only the supplied FEN or current session position.
- Use Stockfish `go depth <depth>` with `MultiPV=<multipv>`.
- Return principal variations in UCI.

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

Long-running tools should report progress:

- `analyze_position`: engine started, depth complete if available, completed

Cancellation should stop the active Stockfish search with `stop`, drain output until `bestmove`, and return `analysis_cancelled`.

## Stockfish Process Requirements

Default paths:

- Windows: `stockfish/stockfish.exe`
- macOS/Linux: `stockfish/stockfish`

Allow override through:

- server config file
- environment variable
- command-line argument

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

The first implementation should include:

- MCP server startup
- Stockfish status check
- board session create/get/set
- make/undo move
- legal move listing
- position analysis
- in-memory cache
- unit tests and one real Stockfish integration test