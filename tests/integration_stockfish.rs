//! Integration test that exercises a real Stockfish process via the public engine API.
//!
//! These tests are marked `#[ignore]` so they don't break developer machines that
//! don't have Stockfish installed. To run them:
//!
//!     cargo test --test integration_stockfish -- --ignored --nocapture
//!
//! The engine is resolved in the same priority order as the production binary:
//! `CHESS_MCP_STOCKFISH_PATH` env var, then the platform default (stockfish/stockfish[.exe]).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;

use chess_pos_analyzer::cache::ScoreKind;
use chess_pos_analyzer::config::{platform_default_stockfish_path, ENV_STOCKFISH_PATH};
use chess_pos_analyzer::engine::{AnalysisRequest, Engine};

fn locate_stockfish() -> Option<PathBuf> {
    if let Ok(p) = std::env::var(ENV_STOCKFISH_PATH) {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let default = platform_default_stockfish_path();
    if default.exists() {
        return Some(default);
    }
    None
}

#[tokio::test]
#[ignore = "requires Stockfish at CHESS_MCP_STOCKFISH_PATH or ./stockfish/stockfish[.exe]"]
async fn analyze_starting_position_with_multipv_3() {
    let path = locate_stockfish().expect(
        "Stockfish not found. Place it at ./stockfish/stockfish[.exe] or set \
         CHESS_MCP_STOCKFISH_PATH before running this test.",
    );

    let engine = Arc::new(Engine::new(path));
    let info = engine.status().await.expect("engine should be available");
    assert!(info.available);
    assert!(!info.name.is_empty(), "engine should report a name");

    let starting_fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    let (_tx, rx) = oneshot::channel::<()>();
    let result = engine
        .analyze(
            AnalysisRequest {
                fen: starting_fen.to_string(),
                depth: 10,
                multipv: 3,
            },
            None,
            rx,
        )
        .await
        .expect("analysis should succeed");

    assert_eq!(result.schema_version, "1.0");
    assert_eq!(result.depth, 10);
    assert_eq!(result.multipv, 3);
    assert!(
        result.lines.len() >= 1 && result.lines.len() <= 3,
        "expected 1..=3 lines, got {}",
        result.lines.len()
    );

    let first = &result.lines[0];
    assert_eq!(first.rank, 1);
    assert!(matches!(
        first.score.kind,
        ScoreKind::Cp | ScoreKind::Mate
    ));
    assert!(
        !first.pv_uci.is_empty(),
        "best line must have at least one PV move"
    );
    // The first PV move at the starting position is from a white piece.
    let opener = &first.pv_uci[0];
    assert!(
        opener.starts_with('e')
            || opener.starts_with('d')
            || opener.starts_with('g')
            || opener.starts_with('b')
            || opener.starts_with('c')
            || opener.starts_with('a')
            || opener.starts_with('h')
            || opener.starts_with('f'),
        "unexpected best move: {}",
        opener
    );

    // Ranks should be unique and 1-based.
    let mut ranks: Vec<u32> = result.lines.iter().map(|l| l.rank).collect();
    ranks.sort();
    for (i, r) in ranks.iter().enumerate() {
        assert_eq!(*r, (i as u32) + 1);
    }
}

#[tokio::test]
#[ignore = "requires Stockfish at CHESS_MCP_STOCKFISH_PATH or ./stockfish/stockfish[.exe]"]
async fn analyze_mate_in_one_finds_mate() {
    let path = locate_stockfish().expect(
        "Stockfish not found. Place it at ./stockfish/stockfish[.exe] or set \
         CHESS_MCP_STOCKFISH_PATH before running this test.",
    );
    let engine = Arc::new(Engine::new(path));

    // Black to move, mate in 1 with Qh4# is unavailable here; use a classic mate-in-one position:
    // 6k1/5ppp/8/8/8/8/5PPP/4R1K1 w - - 0 1 -> Re8#
    let fen = "6k1/5ppp/8/8/8/8/5PPP/4R1K1 w - - 0 1";
    let (_tx, rx) = oneshot::channel::<()>();
    let result = engine
        .analyze(
            AnalysisRequest {
                fen: fen.to_string(),
                depth: 6,
                multipv: 1,
            },
            None,
            rx,
        )
        .await
        .expect("analysis should succeed");

    let first = &result.lines[0];
    assert_eq!(first.score.kind, ScoreKind::Mate);
    assert!(first.score.mate_in.is_some());
    assert_eq!(first.pv_uci[0], "e1e8", "expected Re8# as first move");
}
