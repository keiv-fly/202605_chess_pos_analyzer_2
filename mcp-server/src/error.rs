use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidFen,
    IllegalMove,
    EngineUnavailable,
    EngineTimeout,
    AnalysisCancelled,
    SessionNotFound,
    InvalidArgument,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidFen => "invalid_fen",
            ErrorCode::IllegalMove => "illegal_move",
            ErrorCode::EngineUnavailable => "engine_unavailable",
            ErrorCode::EngineTimeout => "engine_timeout",
            ErrorCode::AnalysisCancelled => "analysis_cancelled",
            ErrorCode::SessionNotFound => "session_not_found",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::Internal => "internal",
        }
    }
}

#[derive(Debug, Error)]
pub enum ChessError {
    #[error("invalid FEN: {message}")]
    InvalidFen { message: String, fen: String },

    #[error("illegal move {attempted}: {message}")]
    IllegalMove {
        attempted: String,
        message: String,
        current_fen: String,
        legal_moves: Vec<String>,
    },

    #[error("Stockfish engine unavailable: {message}")]
    EngineUnavailable {
        message: String,
        expected_path: String,
    },

    #[error("engine timed out after {elapsed_ms} ms at depth {depth}")]
    EngineTimeout {
        depth: u32,
        elapsed_ms: u64,
        partial_available: bool,
    },

    #[error("analysis cancelled")]
    AnalysisCancelled,

    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ChessError {
    pub fn code(&self) -> ErrorCode {
        match self {
            ChessError::InvalidFen { .. } => ErrorCode::InvalidFen,
            ChessError::IllegalMove { .. } => ErrorCode::IllegalMove,
            ChessError::EngineUnavailable { .. } => ErrorCode::EngineUnavailable,
            ChessError::EngineTimeout { .. } => ErrorCode::EngineTimeout,
            ChessError::AnalysisCancelled => ErrorCode::AnalysisCancelled,
            ChessError::SessionNotFound { .. } => ErrorCode::SessionNotFound,
            ChessError::InvalidArgument(_) => ErrorCode::InvalidArgument,
            ChessError::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn details(&self) -> Value {
        match self {
            ChessError::InvalidFen { fen, message } => json!({
                "fen": fen,
                "parser_message": message,
            }),
            ChessError::IllegalMove {
                attempted,
                current_fen,
                legal_moves,
                ..
            } => json!({
                "attempted": attempted,
                "format": "uci",
                "current_fen": current_fen,
                "legal_moves": legal_moves,
            }),
            ChessError::EngineUnavailable { expected_path, .. } => json!({
                "expected_path": expected_path,
                "hint": "Place Stockfish at the expected path or set CHESS_MCP_STOCKFISH_PATH.",
            }),
            ChessError::EngineTimeout {
                depth,
                elapsed_ms,
                partial_available,
            } => json!({
                "depth": depth,
                "elapsed_ms": elapsed_ms,
                "partial_available": partial_available,
            }),
            ChessError::AnalysisCancelled => json!({}),
            ChessError::SessionNotFound { session_id } => json!({ "session_id": session_id }),
            ChessError::InvalidArgument(_) => json!({}),
            ChessError::Internal(_) => json!({}),
        }
    }

    pub fn to_payload(&self) -> Value {
        json!({
            "error": {
                "code": self.code().as_str(),
                "message": self.to_string(),
                "details": self.details(),
            }
        })
    }
}

pub type ChessResult<T> = Result<T, ChessError>;
