use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::board::{BoardSession, BoardSnapshot};
use crate::cache::{AnalysisCache, AnalysisResult, CacheKey};
use crate::config::ResolvedConfig;
use crate::engine::{
    default_uci_options, AnalysisRequest, Engine, EngineInfo, ProgressEvent, DEFAULT_DEPTH,
    DEFAULT_MULTIPV,
};
use crate::error::ChessError;

pub const SESSION_RESOURCE_URI: &str = "chess://session/current";
pub const ENGINE_RESOURCE_URI: &str = "chess://engine/status";

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateBoardArgs {
    /// Starting FEN. When omitted the standard starting position is used.
    #[serde(default)]
    pub fen: Option<String>,
    /// Optional UCI moves applied after the base FEN.
    #[serde(default)]
    pub moves_uci: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionIdArgs {
    pub session_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MakeMoveArgs {
    pub session_id: String,
    /// Move in UCI notation, e.g. `e2e4` or `e7e8q`.
    #[serde(rename = "move")]
    pub mv: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UndoMoveArgs {
    pub session_id: String,
    #[serde(default = "default_plies")]
    pub plies: u32,
}

fn default_plies() -> u32 {
    1
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalyzePositionArgs {
    /// Use the position from this session. Mutually exclusive with `fen`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Analyze this raw FEN. Mutually exclusive with `session_id`.
    #[serde(default)]
    pub fen: Option<String>,
    #[serde(default = "default_depth_arg")]
    pub depth: u32,
    #[serde(default = "default_multipv_arg")]
    pub multipv: u32,
}

fn default_depth_arg() -> u32 {
    DEFAULT_DEPTH
}
fn default_multipv_arg() -> u32 {
    DEFAULT_MULTIPV
}

#[derive(Clone)]
pub struct ChessServer {
    config: Arc<ResolvedConfig>,
    session: Arc<Mutex<Option<BoardSession>>>,
    engine: Arc<Engine>,
    cache: Arc<AnalysisCache>,
    tool_router: ToolRouter<ChessServer>,
}

#[tool_router]
impl ChessServer {
    pub fn new(config: ResolvedConfig) -> Self {
        let engine = Arc::new(Engine::new(config.stockfish_path.clone()));
        let cache = Arc::new(AnalysisCache::new(config.cache_capacity));
        Self {
            config: Arc::new(config),
            session: Arc::new(Mutex::new(None)),
            engine,
            cache,
            tool_router: Self::tool_router(),
        }
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn cache(&self) -> &Arc<AnalysisCache> {
        &self.cache
    }

    pub fn session_handle(&self) -> Arc<Mutex<Option<BoardSession>>> {
        self.session.clone()
    }

    #[tool(
        description = "Create a new chess board session. Replaces any existing active session. \
                       Optionally takes a starting FEN and a list of UCI moves to apply."
    )]
    async fn create_board(
        &self,
        Parameters(args): Parameters<CreateBoardArgs>,
    ) -> Result<CallToolResult, McpError> {
        let CreateBoardArgs { fen, moves_uci } = args;
        match BoardSession::new(fen.as_deref(), &moves_uci) {
            Ok(session) => {
                let snapshot = session.snapshot();
                *self.session.lock().await = Some(session);
                Ok(snapshot_response(&snapshot))
            }
            Err(e) => Ok(error_response(e)),
        }
    }

    #[tool(description = "Return the current state of the active board session.")]
    async fn get_board(
        &self,
        Parameters(args): Parameters<SessionIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let session_guard = self.session.lock().await;
        match session_guard.as_ref() {
            Some(s) if s.id().to_string() == args.session_id => Ok(snapshot_response(&s.snapshot())),
            _ => Ok(error_response(ChessError::SessionNotFound {
                session_id: args.session_id,
            })),
        }
    }

    #[tool(description = "Apply one legal move (UCI format) to the active board session.")]
    async fn make_move(
        &self,
        Parameters(args): Parameters<MakeMoveArgs>,
    ) -> Result<CallToolResult, McpError> {
        let MakeMoveArgs { session_id, mv } = args;
        let mut session_guard = self.session.lock().await;
        let session = match session_guard.as_mut() {
            Some(s) if s.id().to_string() == session_id => s,
            _ => {
                return Ok(error_response(ChessError::SessionNotFound { session_id }));
            }
        };
        match session.make_move(&mv) {
            Ok(_) => Ok(snapshot_response(&session.snapshot())),
            Err(e) => Ok(error_response(e)),
        }
    }

    #[tool(description = "Move backward in the active session's history by `plies` half-moves.")]
    async fn undo_move(
        &self,
        Parameters(args): Parameters<UndoMoveArgs>,
    ) -> Result<CallToolResult, McpError> {
        let UndoMoveArgs { session_id, plies } = args;
        let mut session_guard = self.session.lock().await;
        let session = match session_guard.as_mut() {
            Some(s) if s.id().to_string() == session_id => s,
            _ => {
                return Ok(error_response(ChessError::SessionNotFound { session_id }));
            }
        };
        match session.undo(plies) {
            Ok(_) => Ok(snapshot_response(&session.snapshot())),
            Err(e) => Ok(error_response(e)),
        }
    }

    #[tool(description = "Check Stockfish engine availability and report configured UCI options.")]
    async fn engine_status(&self) -> Result<CallToolResult, McpError> {
        match self.engine.status().await {
            Ok(info) => Ok(json_response(&info)),
            Err(_) => {
                let info = EngineInfo {
                    available: false,
                    path: self.engine.engine_path_string(),
                    name: String::new(),
                    uci_options: default_uci_options(),
                };
                Ok(json_response(&info))
            }
        }
    }

    #[tool(
        description = "Analyze a position with Stockfish. Provide exactly one of session_id or fen. \
                       Returns the top `multipv` lines at the requested `depth`."
    )]
    async fn analyze_position(
        &self,
        Parameters(args): Parameters<AnalyzePositionArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Forward MCP progress notifications, if the client supplied a progress token.
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
        let progress_token = ctx.meta.get_progress_token();
        let peer = ctx.peer.clone();
        let progress_task = tokio::spawn(async move {
            let mut progress: f64 = 0.0;
            while let Some(event) = progress_rx.recv().await {
                let Some(token) = progress_token.as_ref() else { continue };
                let (msg, advance) = match &event {
                    ProgressEvent::Started { depth, multipv } => (
                        format!("engine_started depth={} multipv={}", depth, multipv),
                        0.0,
                    ),
                    ProgressEvent::DepthComplete { depth, .. } => {
                        (format!("depth_complete depth={}", depth), 1.0)
                    }
                    ProgressEvent::Completed => ("completed".to_string(), 0.0),
                };
                progress += advance;
                let _ = peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress,
                        total: None,
                        message: Some(msg),
                    })
                    .await;
            }
        });

        // Wire the request's cancellation token to a oneshot the engine can listen on.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        let ct = ctx.ct.clone();
        let cancel_forward = tokio::spawn(async move {
            ct.cancelled().await;
            let _ = cancel_tx.send(());
        });

        let result = self
            .run_analysis(args, Some(progress_tx), cancel_rx)
            .await;

        cancel_forward.abort();
        let _ = progress_task.await;
        result
    }

    /// Context-free analysis pipeline shared by the MCP tool and by unit tests.
    pub async fn run_analysis(
        &self,
        args: AnalyzePositionArgs,
        progress: Option<mpsc::UnboundedSender<ProgressEvent>>,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<CallToolResult, McpError> {
        let started_at = std::time::Instant::now();
        let AnalyzePositionArgs {
            session_id,
            fen,
            depth,
            multipv,
        } = args;

        if session_id.is_some() == fen.is_some() {
            return Ok(error_response(ChessError::InvalidArgument(
                "exactly one of `session_id` or `fen` must be supplied".into(),
            )));
        }
        if depth == 0 {
            return Ok(error_response(ChessError::InvalidArgument(
                "`depth` must be at least 1".into(),
            )));
        }
        if multipv == 0 {
            return Ok(error_response(ChessError::InvalidArgument(
                "`multipv` must be at least 1".into(),
            )));
        }

        let fen_str = if let Some(sid) = session_id {
            let session_guard = self.session.lock().await;
            match session_guard.as_ref() {
                Some(s) if s.id().to_string() == sid => s.fen(),
                _ => {
                    return Ok(error_response(ChessError::SessionNotFound { session_id: sid }));
                }
            }
        } else {
            fen.unwrap()
        };

        let cache_key = match CacheKey::new(&fen_str, depth, multipv) {
            Ok(k) => k,
            Err(e) => return Ok(error_response(e)),
        };

        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(analysis_response(&cached, started_at.elapsed()));
        }

        let result = self
            .engine
            .analyze(
                AnalysisRequest {
                    fen: fen_str.clone(),
                    depth,
                    multipv,
                },
                progress,
                cancel_rx,
            )
            .await;

        match result {
            Ok(result) => {
                self.cache.put(cache_key, result.clone());
                Ok(analysis_response(&result, started_at.elapsed()))
            }
            Err(e) => Ok(error_response(e)),
        }
    }
}

fn snapshot_response(snap: &BoardSnapshot) -> CallToolResult {
    json_response(snap)
}

fn json_response<T: Serialize>(value: &T) -> CallToolResult {
    let payload = serde_json::to_string_pretty(value)
        .unwrap_or_else(|e| format!("{{\"error\":{{\"code\":\"internal\",\"message\":{:?}}}}}", e.to_string()));
    CallToolResult::success(vec![Content::text(payload)])
}

/// Serializes an `AnalysisResult` and injects the server-side time spent
/// delivering it (engine work for fresh results, ~0 for cache hits), rounded
/// to one decimal place in seconds. Not part of the cached value.
fn analysis_response(result: &AnalysisResult, elapsed: std::time::Duration) -> CallToolResult {
    let mut value = match serde_json::to_value(result) {
        Ok(v) => v,
        Err(e) => {
            return CallToolResult::success(vec![Content::text(format!(
                "{{\"error\":{{\"code\":\"internal\",\"message\":{:?}}}}}",
                e.to_string()
            ))]);
        }
    };
    if let Some(obj) = value.as_object_mut() {
        let elapsed_seconds = (elapsed.as_secs_f64() * 10.0).round() / 10.0;
        obj.insert("elapsed_seconds".into(), json!(elapsed_seconds));
    }
    let payload = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|e| format!("{{\"error\":{{\"code\":\"internal\",\"message\":{:?}}}}}", e.to_string()));
    CallToolResult::success(vec![Content::text(payload)])
}

fn error_response(err: ChessError) -> CallToolResult {
    let payload = err.to_payload();
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    CallToolResult {
        content: vec![Content::text(text)],
        structured_content: None,
        is_error: Some(true),
        meta: None,
    }
}

#[tool_handler]
impl ServerHandler for ChessServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Local chess position analyzer. Tools: create_board, get_board, make_move, \
                 undo_move, engine_status, analyze_position. Resources: chess://session/current, \
                 chess://engine/status. No opening-book or online lookups."
                    .to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let session_res = RawResource::new(SESSION_RESOURCE_URI, "Active board session")
            .no_annotation();
        let engine_res = RawResource::new(ENGINE_RESOURCE_URI, "Stockfish engine status")
            .no_annotation();
        Ok(ListResourcesResult {
            resources: vec![session_res, engine_res],
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        ReadResourceRequestParam { uri }: ReadResourceRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match uri.as_str() {
            SESSION_RESOURCE_URI => {
                let snap = self.session.lock().await.as_ref().map(BoardSession::snapshot);
                let body = match snap {
                    Some(s) => serde_json::to_string_pretty(&s).unwrap_or_else(|_| "{}".into()),
                    None => json!({ "active": false }).to_string(),
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(body, uri)],
                })
            }
            ENGINE_RESOURCE_URI => {
                let body = match self.engine.status().await {
                    Ok(info) => serde_json::to_string_pretty(&info).unwrap_or_default(),
                    Err(e) => serde_json::to_string_pretty(&Value::from(e.to_payload()))
                        .unwrap_or_default(),
                };
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(body, uri)],
                })
            }
            _ => Err(McpError::resource_not_found(
                "resource_not_found",
                Some(json!({ "uri": uri })),
            )),
        }
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use std::path::PathBuf;

    fn test_config() -> ResolvedConfig {
        ResolvedConfig {
            stockfish_path: PathBuf::from("/nonexistent/stockfish"),
            cache_capacity: 8,
            log_level: "info".into(),
        }
    }

    #[tokio::test]
    async fn create_board_replaces_previous_session() {
        let server = ChessServer::new(test_config());
        let first = server
            .create_board(Parameters(CreateBoardArgs {
                fen: None,
                moves_uci: vec![],
            }))
            .await
            .unwrap();
        let first_id = extract_session_id(&first);

        let second = server
            .create_board(Parameters(CreateBoardArgs {
                fen: None,
                moves_uci: vec!["e2e4".into()],
            }))
            .await
            .unwrap();
        let second_id = extract_session_id(&second);
        assert_ne!(first_id, second_id);

        // First session id should now be invalid.
        let stale = server
            .get_board(Parameters(SessionIdArgs {
                session_id: first_id.clone(),
            }))
            .await
            .unwrap();
        assert!(is_error_with_code(&stale, "session_not_found"));
    }

    #[tokio::test]
    async fn get_board_returns_current_snapshot() {
        let server = ChessServer::new(test_config());
        let create = server
            .create_board(Parameters(CreateBoardArgs {
                fen: None,
                moves_uci: vec![],
            }))
            .await
            .unwrap();
        let sid = extract_session_id(&create);
        let got = server
            .get_board(Parameters(SessionIdArgs {
                session_id: sid.clone(),
            }))
            .await
            .unwrap();
        let payload = extract_text(&got);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["session_id"], sid);
        assert_eq!(v["turn"], "white");
    }

    #[tokio::test]
    async fn make_move_with_bad_id_returns_session_not_found() {
        let server = ChessServer::new(test_config());
        let result = server
            .make_move(Parameters(MakeMoveArgs {
                session_id: "not-a-real-uuid".into(),
                mv: "e2e4".into(),
            }))
            .await
            .unwrap();
        assert!(is_error_with_code(&result, "session_not_found"));
    }

    #[tokio::test]
    async fn make_move_illegal_returns_illegal_move() {
        let server = ChessServer::new(test_config());
        let create = server
            .create_board(Parameters(CreateBoardArgs {
                fen: None,
                moves_uci: vec![],
            }))
            .await
            .unwrap();
        let sid = extract_session_id(&create);
        let res = server
            .make_move(Parameters(MakeMoveArgs {
                session_id: sid,
                mv: "e2e5".into(),
            }))
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "illegal_move"));
    }

    #[tokio::test]
    async fn undo_move_walks_back() {
        let server = ChessServer::new(test_config());
        let create = server
            .create_board(Parameters(CreateBoardArgs {
                fen: None,
                moves_uci: vec!["e2e4".into(), "e7e5".into()],
            }))
            .await
            .unwrap();
        let sid = extract_session_id(&create);
        let after = server
            .undo_move(Parameters(UndoMoveArgs {
                session_id: sid,
                plies: 1,
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(&after)).unwrap();
        assert_eq!(v["history"], serde_json::json!(["e2e4"]));
        assert_eq!(v["turn"], "black");
    }

    #[tokio::test]
    async fn engine_status_reports_unavailable_when_missing() {
        let server = ChessServer::new(test_config());
        let res = server.engine_status().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(&res)).unwrap();
        assert_eq!(v["available"], false);
    }

    #[tokio::test]
    async fn analyze_requires_exactly_one_of_session_or_fen() {
        let server = ChessServer::new(test_config());

        // Both supplied
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: Some("x".into()),
                    fen: Some("y".into()),
                    depth: 10,
                    multipv: 1,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "invalid_argument"));

        // Neither supplied
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: None,
                    fen: None,
                    depth: 10,
                    multipv: 1,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "invalid_argument"));
    }

    #[tokio::test]
    async fn analyze_rejects_zero_depth_or_multipv() {
        let server = ChessServer::new(test_config());
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: None,
                    fen: Some(
                        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".into(),
                    ),
                    depth: 0,
                    multipv: 1,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "invalid_argument"));
    }

    #[tokio::test]
    async fn analyze_returns_engine_unavailable_when_no_engine() {
        let server = ChessServer::new(test_config());
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: None,
                    fen: Some(
                        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".into(),
                    ),
                    depth: 10,
                    multipv: 1,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "engine_unavailable"));
    }

    #[tokio::test]
    async fn analyze_returns_cached_without_engine_call() {
        use crate::cache::AnalysisResult;
        let server = ChessServer::new(test_config());
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let key = CacheKey::new(fen, 12, 2).unwrap();
        let cached = AnalysisResult {
            schema_version: "1.0".into(),
            fen: fen.to_string(),
            depth: 12,
            multipv: 2,
            lines: vec![],
        };
        server.cache.put(key, cached.clone());
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: None,
                    fen: Some(fen.into()),
                    depth: 12,
                    multipv: 2,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(&res)).unwrap();
        assert_eq!(v["depth"], 12);
        assert_eq!(v["fen"], fen);
    }

    #[tokio::test]
    async fn analyze_with_unknown_session_id_returns_session_not_found() {
        let server = ChessServer::new(test_config());
        let (_tx, rx) = oneshot::channel::<()>();
        let res = server
            .run_analysis(
                AnalyzePositionArgs {
                    session_id: Some("bogus".into()),
                    fen: None,
                    depth: 10,
                    multipv: 1,
                },
                None,
                rx,
            )
            .await
            .unwrap();
        assert!(is_error_with_code(&res, "session_not_found"));
    }

    fn extract_text(result: &CallToolResult) -> String {
        let content = result
            .content
            .first()
            .expect("expected at least one content block");
        match &content.raw {
            RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        }
    }

    fn extract_session_id(result: &CallToolResult) -> String {
        let text = extract_text(result);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["session_id"].as_str().unwrap().to_string()
    }

    fn is_error_with_code(result: &CallToolResult, code: &str) -> bool {
        if result.is_error != Some(true) {
            return false;
        }
        let text = extract_text(result);
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return false,
        };
        v["error"]["code"].as_str() == Some(code)
    }
}
