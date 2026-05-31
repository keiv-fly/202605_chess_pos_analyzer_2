use crate::error::ChessError;

/// Normalize a FEN string for caching purposes.
///
/// The cache key drops the halfmove clock and fullmove number, keeping:
///   piece placement, side to move, castling rights, en-passant square.
pub fn normalize_fen(fen: &str) -> Result<String, ChessError> {
    let parts: Vec<&str> = fen.split_ascii_whitespace().collect();
    if parts.len() < 4 {
        return Err(ChessError::InvalidFen {
            message: format!(
                "FEN must contain at least 4 fields, got {}",
                parts.len()
            ),
            fen: fen.to_string(),
        });
    }
    Ok(format!(
        "{} {} {} {}",
        parts[0], parts[1], parts[2], parts[3]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_halfmove_and_fullmove() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let normalized = normalize_fen(fen).unwrap();
        assert_eq!(
            normalized,
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -"
        );
    }

    #[test]
    fn accepts_already_truncated_fen() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -";
        let normalized = normalize_fen(fen).unwrap();
        assert_eq!(
            normalized,
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq -"
        );
    }

    #[test]
    fn rejects_too_few_fields() {
        let fen = "rnbqkbnr/pppppppp w";
        let err = normalize_fen(fen).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidFen);
    }

    #[test]
    fn rejects_empty() {
        let err = normalize_fen("").unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidFen);
    }

    #[test]
    fn two_positions_differing_only_in_clocks_normalize_equal() {
        let a = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let b = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 5 12";
        assert_eq!(normalize_fen(a).unwrap(), normalize_fen(b).unwrap());
    }
}
