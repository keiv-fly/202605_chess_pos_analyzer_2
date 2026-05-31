use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::san::SanPlus;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, Position, Role};

use crate::error::ChessError;

/// A single capture event extracted from a principal variation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CaptureEvent {
    /// 1-indexed position in the PV.
    pub ply: u32,
    /// "white" or "black" — the side making the capture.
    pub side: String,
    /// Single-letter role of the capturing piece (P/N/B/R/Q/K).
    pub piece: String,
    /// Single-letter role of the captured piece.
    pub captured: String,
    /// Destination square in algebraic notation (e.g. "e4").
    pub square: String,
    /// Conventional point value of the captured piece (P=1, N=B=3, R=5, Q=9).
    pub value: i32,
    /// True for en-passant captures (the captured pawn is not on `square`).
    #[serde(default)]
    pub en_passant: bool,
}

/// Net material change after playing the PV, from the side-to-move's perspective.
///
/// `white_lost` and `black_lost` count point values of pieces removed by captures
/// in the PV (a piece that is recaptured contributes to both). `net_for_stm` is
/// positive when the side that moved first comes out ahead in those swaps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MaterialSwing {
    pub white_lost: i32,
    pub black_lost: i32,
    pub net_for_stm: i32,
}

#[derive(Debug, Clone)]
pub struct PvExplanation {
    pub pv_san: Vec<String>,
    pub captures: Vec<CaptureEvent>,
    pub material_swing: MaterialSwing,
}

pub fn piece_value(role: Role) -> i32 {
    match role {
        Role::Pawn => 1,
        Role::Knight | Role::Bishop => 3,
        Role::Rook => 5,
        Role::Queen => 9,
        Role::King => 0,
    }
}

pub fn role_char(role: Role) -> &'static str {
    match role {
        Role::Pawn => "P",
        Role::Knight => "N",
        Role::Bishop => "B",
        Role::Rook => "R",
        Role::Queen => "Q",
        Role::King => "K",
    }
}

fn color_str(c: Color) -> &'static str {
    match c {
        Color::White => "white",
        Color::Black => "black",
    }
}

fn parse_position(fen_str: &str) -> Result<Chess, ChessError> {
    let fen = Fen::from_ascii(fen_str.as_bytes()).map_err(|e| ChessError::InvalidFen {
        message: e.to_string(),
        fen: fen_str.to_string(),
    })?;
    fen.into_position::<Chess>(CastlingMode::Standard)
        .map_err(|e| ChessError::InvalidFen {
            message: format!("{:?}", e),
            fen: fen_str.to_string(),
        })
}

/// Walk a principal variation from `fen` and produce SAN + capture/material
/// annotations. Returns an empty explanation if `pv_uci` is empty.
///
/// Returns an error only on FEN parse failure or if a PV move is not legal in
/// the position it would be played in (which would indicate engine/parsing
/// disagreement and is worth surfacing).
pub fn explain_pv(fen: &str, pv_uci: &[String]) -> Result<PvExplanation, ChessError> {
    let mut pos = parse_position(fen)?;
    let stm_color = pos.turn();
    let mut pv_san: Vec<String> = Vec::with_capacity(pv_uci.len());
    let mut captures: Vec<CaptureEvent> = Vec::new();
    let mut swing = MaterialSwing::default();

    for (i, mv_str) in pv_uci.iter().enumerate() {
        let parsed = UciMove::from_ascii(mv_str.as_bytes()).map_err(|e| {
            ChessError::Internal(format!("pv move not parseable: '{}' ({})", mv_str, e))
        })?;
        let mv = parsed.to_move(&pos).map_err(|_| {
            ChessError::Internal(format!(
                "pv move '{}' not legal in intermediate position",
                mv_str
            ))
        })?;

        if mv.is_capture() {
            let mover_side = pos.turn();
            let captured_role = mv.capture().unwrap_or(Role::Pawn);
            let value = piece_value(captured_role);
            captures.push(CaptureEvent {
                ply: (i + 1) as u32,
                side: color_str(mover_side).to_string(),
                piece: role_char(mv.role()).to_string(),
                captured: role_char(captured_role).to_string(),
                square: mv.to().to_string(),
                value,
                en_passant: mv.is_en_passant(),
            });
            match mover_side {
                Color::White => swing.black_lost += value,
                Color::Black => swing.white_lost += value,
            }
        }

        let san = SanPlus::from_move(pos.clone(), &mv);
        pv_san.push(san.to_string());

        pos = pos.play(&mv).map_err(|_| {
            ChessError::Internal(format!("could not play pv move '{}'", mv_str))
        })?;
    }

    swing.net_for_stm = match stm_color {
        Color::White => swing.black_lost - swing.white_lost,
        Color::Black => swing.white_lost - swing.black_lost,
    };

    Ok(PvExplanation {
        pv_san,
        captures,
        material_swing: swing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTING_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn empty_pv_yields_empty_explanation() {
        let e = explain_pv(STARTING_FEN, &[]).unwrap();
        assert!(e.pv_san.is_empty());
        assert!(e.captures.is_empty());
        assert_eq!(e.material_swing, MaterialSwing::default());
    }

    #[test]
    fn quiet_pv_has_no_captures() {
        let pv = vec!["e2e4".to_string(), "e7e5".to_string(), "g1f3".to_string()];
        let e = explain_pv(STARTING_FEN, &pv).unwrap();
        assert_eq!(e.pv_san, vec!["e4", "e5", "Nf3"]);
        assert!(e.captures.is_empty());
        assert_eq!(e.material_swing.net_for_stm, 0);
    }

    #[test]
    fn capture_recorded_with_value_and_square() {
        // After 1.e4 e5 2.Nf3 Nc6 3.Bb5 a6 4.Bxc6 -- white wins a knight (value 3).
        let pv = vec![
            "e2e4".into(),
            "e7e5".into(),
            "g1f3".into(),
            "b8c6".into(),
            "f1b5".into(),
            "a7a6".into(),
            "b5c6".into(),
        ];
        let e = explain_pv(STARTING_FEN, &pv).unwrap();
        assert_eq!(e.pv_san.last().unwrap(), "Bxc6");
        assert_eq!(e.captures.len(), 1);
        let cap = &e.captures[0];
        assert_eq!(cap.ply, 7);
        assert_eq!(cap.side, "white");
        assert_eq!(cap.piece, "B");
        assert_eq!(cap.captured, "N");
        assert_eq!(cap.square, "c6");
        assert_eq!(cap.value, 3);
        assert!(!cap.en_passant);
        // White is side-to-move at depth 0, white captured a knight ⇒ +3.
        assert_eq!(e.material_swing.white_lost, 0);
        assert_eq!(e.material_swing.black_lost, 3);
        assert_eq!(e.material_swing.net_for_stm, 3);
    }

    #[test]
    fn rook_for_queen_trade_summarized() {
        // The puzzle position from the conversation.
        let fen = "r1b1k1r1/pp2pp2/2pp3b/5PNn/N1PPP1Q1/3B4/PP4P1/2KR4 b q - 0 18";
        // Engine's main line: 18...Rxg5 19.Qe2 Rxg2 20.Kb1 Rxe2 21.Bxe2
        let pv = vec![
            "g8g5".into(),
            "g4e2".into(),
            "g5g2".into(),
            "c1b1".into(),
            "g2e2".into(),
            "d3e2".into(),
        ];
        let e = explain_pv(fen, &pv).unwrap();
        // Captures: Rxg5 (N), Rxg2 with discovered check from Bh6, Rxe2 (Q), Bxe2 (R).
        // SAN annotates the discovered check, which is the tactical glue: White
        // can't recapture with Qxg2 because the king is in check.
        assert_eq!(
            e.pv_san,
            vec!["Rxg5", "Qe2", "Rxg2+", "Kb1", "Rxe2", "Bxe2"]
        );
        let captured_pieces: Vec<&str> = e.captures.iter().map(|c| c.captured.as_str()).collect();
        assert_eq!(captured_pieces, vec!["N", "P", "Q", "R"]);
        // Black (stm) captured N+P+Q = 13, lost R = 5. Net = +8 in this swap.
        assert_eq!(e.material_swing.white_lost, 3 + 1 + 9);
        assert_eq!(e.material_swing.black_lost, 5);
        assert_eq!(e.material_swing.net_for_stm, 8);
    }

    #[test]
    fn en_passant_capture_flagged() {
        // White plays e2-e4, then black plays d4-... position with en passant.
        // Build a position where en passant is available.
        let fen = "rnbqkbnr/ppp1pppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 2";
        let pv = vec!["d4e3".into()];
        let e = explain_pv(fen, &pv).unwrap();
        assert_eq!(e.captures.len(), 1);
        assert!(e.captures[0].en_passant);
        assert_eq!(e.captures[0].captured, "P");
        assert_eq!(e.captures[0].value, 1);
    }

    #[test]
    fn illegal_pv_move_returns_error() {
        let err = explain_pv(STARTING_FEN, &["e2e5".to_string()]).unwrap_err();
        // It's an Internal because PV legality is an invariant, not user input.
        match err {
            ChessError::Internal(_) => {}
            other => panic!("expected Internal, got {:?}", other),
        }
    }

    #[test]
    fn bad_fen_returns_invalid_fen() {
        let err = explain_pv("not a fen", &[]).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidFen);
    }
}
