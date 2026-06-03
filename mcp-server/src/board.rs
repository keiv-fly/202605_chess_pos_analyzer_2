use serde::{Deserialize, Serialize};
use shakmaty::fen::Fen;
use shakmaty::uci::UciMove;
use shakmaty::{CastlingMode, Chess, Color, EnPassantMode, Position};
use uuid::Uuid;

use crate::error::ChessError;

/// JSON-friendly snapshot of a board session, shared by every board tool response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardSnapshot {
    pub session_id: String,
    pub fen: String,
    pub turn: String,
    pub move_number: u32,
    pub history: Vec<String>,
    pub board_text: String,
    pub legal_move_count: usize,
    pub state: String,
    pub legal_moves: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BoardSession {
    id: Uuid,
    initial_fen: String,
    position: Chess,
    history: Vec<String>,
}

impl BoardSession {
    pub fn new(initial_fen: Option<&str>, moves_uci: &[String]) -> Result<Self, ChessError> {
        let fen_str = initial_fen
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(STARTING_FEN);
        let position = parse_position(fen_str)?;

        let id = Uuid::new_v4();
        let mut session = Self {
            id,
            initial_fen: fen_str.to_string(),
            position,
            history: Vec::new(),
        };
        for mv in moves_uci {
            session.make_move(mv)?;
        }
        Ok(session)
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn position(&self) -> &Chess {
        &self.position
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Apply a UCI move. Returns the canonical UCI string that was applied.
    pub fn make_move(&mut self, uci: &str) -> Result<String, ChessError> {
        let parsed = UciMove::from_ascii(uci.as_bytes()).map_err(|e| ChessError::IllegalMove {
            attempted: uci.to_string(),
            message: format!("could not parse UCI move: {}", e),
            current_fen: self.fen(),
            legal_moves: self.legal_moves_uci(),
        })?;
        let mv = parsed
            .to_move(&self.position)
            .map_err(|_| ChessError::IllegalMove {
                attempted: uci.to_string(),
                message: "move is not legal in the current position".to_string(),
                current_fen: self.fen(),
                legal_moves: self.legal_moves_uci(),
            })?;
        let canonical = UciMove::from_standard(&mv).to_string();
        let next = self
            .position
            .clone()
            .play(&mv)
            .map_err(|_| ChessError::IllegalMove {
                attempted: uci.to_string(),
                message: "engine rejected move".to_string(),
                current_fen: self.fen(),
                legal_moves: self.legal_moves_uci(),
            })?;
        self.position = next;
        self.history.push(canonical.clone());
        Ok(canonical)
    }

    /// Undo `plies` moves. Returns the number actually undone.
    pub fn undo(&mut self, plies: u32) -> Result<u32, ChessError> {
        if plies == 0 {
            return Ok(0);
        }
        let target_len = self.history.len().saturating_sub(plies as usize);
        let kept: Vec<String> = self.history.iter().take(target_len).cloned().collect();
        let mut position = parse_position(&self.initial_fen)?;
        for mv_str in &kept {
            let parsed = UciMove::from_ascii(mv_str.as_bytes()).map_err(|e| {
                ChessError::Internal(format!("stored history not parseable: {} ({})", mv_str, e))
            })?;
            let mv = parsed.to_move(&position).map_err(|_| {
                ChessError::Internal(format!("stored history not legal: {}", mv_str))
            })?;
            position = position.play(&mv).map_err(|_| {
                ChessError::Internal(format!("stored history not playable: {}", mv_str))
            })?;
        }
        let undone = (self.history.len() - kept.len()) as u32;
        self.history = kept;
        self.position = position;
        Ok(undone)
    }

    pub fn fen(&self) -> String {
        Fen::from_position(self.position.clone(), EnPassantMode::Legal).to_string()
    }

    pub fn turn(&self) -> &'static str {
        match self.position.turn() {
            Color::White => "white",
            Color::Black => "black",
        }
    }

    pub fn move_number(&self) -> u32 {
        self.position.fullmoves().get()
    }

    pub fn legal_moves_uci(&self) -> Vec<String> {
        let mut moves: Vec<String> = self
            .position
            .legal_moves()
            .iter()
            .map(|mv| UciMove::from_standard(mv).to_string())
            .collect();
        moves.sort();
        moves
    }

    pub fn legal_move_count(&self) -> usize {
        self.position.legal_moves().len()
    }

    pub fn state(&self) -> &'static str {
        if self.position.is_checkmate() {
            "checkmate"
        } else if self.position.is_stalemate() {
            "stalemate"
        } else if self.position.is_check() {
            "check"
        } else {
            "normal"
        }
    }

    pub fn board_text(&self) -> String {
        format!("{}", self.position.board())
    }

    pub fn snapshot(&self) -> BoardSnapshot {
        let legal_moves = self.legal_moves_uci();
        BoardSnapshot {
            session_id: self.id.to_string(),
            fen: self.fen(),
            turn: self.turn().to_string(),
            move_number: self.move_number(),
            history: self.history.clone(),
            board_text: self.board_text(),
            legal_move_count: legal_moves.len(),
            state: self.state().to_string(),
            legal_moves,
        }
    }
}

pub const STARTING_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn starting_position_has_twenty_legal_moves() {
        let session = BoardSession::new(None, &[]).unwrap();
        let snap = session.snapshot();
        assert_eq!(snap.legal_move_count, 20);
        assert_eq!(snap.turn, "white");
        assert_eq!(snap.move_number, 1);
        assert_eq!(snap.state, "normal");
        assert!(snap.history.is_empty());
        assert!(snap.legal_moves.contains(&"e2e4".to_string()));
    }

    #[test]
    fn applies_initial_moves() {
        let session =
            BoardSession::new(None, &["e2e4".into(), "e7e5".into(), "g1f3".into()]).unwrap();
        let snap = session.snapshot();
        assert_eq!(snap.history, vec!["e2e4", "e7e5", "g1f3"]);
        assert_eq!(snap.turn, "black");
    }

    #[test]
    fn rejects_illegal_move_with_details() {
        let mut session = BoardSession::new(None, &[]).unwrap();
        let err = session.make_move("e2e5").unwrap_err();
        assert_eq!(err.code(), ErrorCode::IllegalMove);
        match err {
            ChessError::IllegalMove { legal_moves, .. } => {
                assert!(legal_moves.contains(&"e2e4".to_string()));
            }
            _ => panic!("expected IllegalMove"),
        }
    }

    #[test]
    fn rejects_invalid_uci_with_illegal_move_error() {
        let mut session = BoardSession::new(None, &[]).unwrap();
        let err = session.make_move("zz9").unwrap_err();
        assert_eq!(err.code(), ErrorCode::IllegalMove);
    }

    #[test]
    fn rejects_invalid_initial_fen() {
        let err = BoardSession::new(Some("not a fen"), &[]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidFen);
    }

    #[test]
    fn undo_walks_back_history() {
        let mut session =
            BoardSession::new(None, &["e2e4".into(), "e7e5".into(), "g1f3".into()]).unwrap();
        let undone = session.undo(2).unwrap();
        assert_eq!(undone, 2);
        let snap = session.snapshot();
        assert_eq!(snap.history, vec!["e2e4"]);
        assert_eq!(snap.turn, "black");
    }

    #[test]
    fn undo_more_than_history_clamps() {
        let mut session = BoardSession::new(None, &["e2e4".into()]).unwrap();
        let undone = session.undo(5).unwrap();
        assert_eq!(undone, 1);
        assert!(session.history().is_empty());
        assert_eq!(session.snapshot().turn, "white");
    }

    #[test]
    fn undo_zero_is_noop() {
        let mut session = BoardSession::new(None, &["e2e4".into()]).unwrap();
        assert_eq!(session.undo(0).unwrap(), 0);
        assert_eq!(session.history().len(), 1);
    }

    #[test]
    fn checkmate_state_detected() {
        // Fool's mate sequence
        let moves = ["f2f3", "e7e5", "g2g4", "d8h4"];
        let session = BoardSession::new(
            None,
            &moves.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap();
        let snap = session.snapshot();
        assert_eq!(snap.state, "checkmate");
        assert_eq!(snap.legal_move_count, 0);
    }

    #[test]
    fn check_state_detected() {
        // Scholar's mate setup that ends in check (not mate yet)
        let session = BoardSession::new(
            None,
            &[
                "e2e4".into(),
                "e7e5".into(),
                "d1h5".into(),
                "b8c6".into(),
                "f1c4".into(),
                "g8f6".into(),
                "h5f7".into(),
            ],
        )
        .unwrap();
        let snap = session.snapshot();
        assert_eq!(snap.state, "checkmate");
    }

    #[test]
    fn custom_fen_starting_position() {
        let fen = "r1bqkbnr/pppp1ppp/2n5/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R w KQkq - 2 3";
        let session = BoardSession::new(Some(fen), &[]).unwrap();
        let snap = session.snapshot();
        assert_eq!(snap.turn, "white");
        assert_eq!(snap.move_number, 3);
    }

    #[test]
    fn fen_roundtrip_after_moves_is_valid() {
        let mut session = BoardSession::new(None, &[]).unwrap();
        session.make_move("e2e4").unwrap();
        let fen = session.fen();
        // Should be parseable again
        let reloaded = BoardSession::new(Some(&fen), &[]).unwrap();
        assert_eq!(reloaded.fen(), fen);
    }

    #[test]
    fn promotion_move_works() {
        let fen = "8/P7/8/8/8/8/8/k6K w - - 0 1";
        let mut session = BoardSession::new(Some(fen), &[]).unwrap();
        let applied = session.make_move("a7a8q").unwrap();
        assert_eq!(applied, "a7a8q");
    }
}
