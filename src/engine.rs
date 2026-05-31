use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::cache::{AnalysisLine, AnalysisResult, Score, ScoreKind, SCHEMA_VERSION};
use crate::error::ChessError;
use crate::pv_explain::explain_pv;

pub const DEFAULT_DEPTH: u32 = 20;
pub const DEFAULT_MULTIPV: u32 = 6;
pub const DEFAULT_THREADS: u32 = 3;
pub const DEFAULT_HASH_MB: u32 = 128;
pub const ENGINE_TIMEOUT: Duration = Duration::from_secs(300);

/// Default UCI options applied on every fresh engine handshake.
pub fn default_uci_options() -> BTreeMap<String, String> {
    let mut opts = BTreeMap::new();
    opts.insert("Threads".to_string(), DEFAULT_THREADS.to_string());
    opts.insert("Hash".to_string(), DEFAULT_HASH_MB.to_string());
    opts.insert("UCI_AnalyseMode".to_string(), "true".to_string());
    opts.insert("Contempt".to_string(), "0".to_string());
    opts
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub available: bool,
    pub path: String,
    pub name: String,
    pub uci_options: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    Started { depth: u32, multipv: u32 },
    DepthComplete {
        depth: u32,
        best_line: Option<AnalysisLine>,
    },
    Completed,
}

#[derive(Debug)]
pub struct AnalysisRequest {
    pub fen: String,
    pub depth: u32,
    pub multipv: u32,
}

pub struct Engine {
    path: PathBuf,
    inner: Mutex<Option<Spawned>>,
}

struct Spawned {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    name: String,
    uci_options: BTreeMap<String, String>,
    reader_rx: mpsc::UnboundedReceiver<String>,
}

impl Engine {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            inner: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn engine_path_string(&self) -> String {
        self.path.display().to_string()
    }

    /// Lazily ensure the child process is running and return information about it.
    pub async fn status(self: &Arc<Self>) -> Result<EngineInfo, ChessError> {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let s = guard.as_ref().expect("just spawned");
        Ok(EngineInfo {
            available: true,
            path: self.path.display().to_string(),
            name: s.name.clone(),
            uci_options: s.uci_options.clone(),
        })
    }

    /// Run an analysis. `cancel_rx` resolving triggers a UCI `stop` and yields `AnalysisCancelled`.
    pub async fn analyze(
        self: &Arc<Self>,
        req: AnalysisRequest,
        progress: Option<mpsc::UnboundedSender<ProgressEvent>>,
        mut cancel_rx: oneshot::Receiver<()>,
    ) -> Result<AnalysisResult, ChessError> {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let spawned = guard.as_mut().expect("spawned");

        send_line(
            &mut spawned.stdin,
            &format!("setoption name MultiPV value {}", req.multipv),
        )
        .await?;
        spawned
            .uci_options
            .insert("MultiPV".to_string(), req.multipv.to_string());

        send_line(&mut spawned.stdin, "isready").await?;
        wait_for(&mut spawned.reader_rx, |l| l == "readyok", ENGINE_TIMEOUT)
            .await
            .map_err(|_| ChessError::EngineTimeout {
                depth: req.depth,
                elapsed_ms: ENGINE_TIMEOUT.as_millis() as u64,
                partial_available: false,
            })?;

        send_line(&mut spawned.stdin, &format!("position fen {}", req.fen)).await?;
        send_line(&mut spawned.stdin, &format!("go depth {}", req.depth)).await?;

        if let Some(tx) = &progress {
            let _ = tx.send(ProgressEvent::Started {
                depth: req.depth,
                multipv: req.multipv,
            });
        }

        let start = Instant::now();
        let mut current: BTreeMap<u32, AnalysisLine> = BTreeMap::new();
        let mut last_complete_depth: u32 = 0;
        let mut cancelled = false;

        loop {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    if !cancelled {
                        cancelled = true;
                        let _ = send_line(&mut spawned.stdin, "stop").await;
                    }
                }
                line = recv_with_timeout(&mut spawned.reader_rx, ENGINE_TIMEOUT) => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            return Err(ChessError::EngineUnavailable {
                                message: "engine closed unexpectedly".into(),
                                expected_path: self.path.display().to_string(),
                            });
                        }
                        Err(_) => {
                            return Err(ChessError::EngineTimeout {
                                depth: req.depth,
                                elapsed_ms: start.elapsed().as_millis() as u64,
                                partial_available: !current.is_empty(),
                            });
                        }
                    };
                    if let Some(parsed) = parse_info_line(&line) {
                        let prev_depth = last_complete_depth;
                        current.insert(parsed.multipv, parsed.line.clone());
                        if parsed.depth > prev_depth {
                            last_complete_depth = parsed.depth;
                            if let Some(tx) = &progress {
                                let _ = tx.send(ProgressEvent::DepthComplete {
                                    depth: parsed.depth,
                                    best_line: current.get(&1).cloned(),
                                });
                            }
                        }
                    } else if line.starts_with("bestmove") {
                        break;
                    }
                }
            }
        }

        if cancelled {
            return Err(ChessError::AnalysisCancelled);
        }

        if let Some(tx) = &progress {
            let _ = tx.send(ProgressEvent::Completed);
        }

        let mut lines: Vec<AnalysisLine> = current.into_values().collect();
        lines.sort_by_key(|l| l.rank);

        // Enrich each line with SAN, capture events, and material swing.
        // Skip silently on per-line failure: engines occasionally emit a PV
        // we can't replay (encoding edge cases), and that shouldn't sink the
        // whole analysis — the raw UCI line is still useful.
        for line in &mut lines {
            if line.pv_uci.is_empty() {
                continue;
            }
            if let Ok(exp) = explain_pv(&req.fen, &line.pv_uci) {
                line.pv_san = exp.pv_san;
                line.captures = exp.captures;
                line.material_swing = exp.material_swing;
            }
        }

        Ok(AnalysisResult {
            schema_version: SCHEMA_VERSION.to_string(),
            fen: req.fen,
            depth: req.depth,
            multipv: req.multipv,
            lines,
        })
    }

    async fn spawn(&self) -> Result<Spawned, ChessError> {
        if !self.path.exists() {
            return Err(ChessError::EngineUnavailable {
                message: format!("Stockfish executable not found at {:?}", self.path),
                expected_path: self.path.display().to_string(),
            });
        }
        let mut child = Command::new(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ChessError::EngineUnavailable {
                message: format!("failed to spawn Stockfish: {}", e),
                expected_path: self.path.display().to_string(),
            })?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ChessError::Internal("failed to open Stockfish stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ChessError::Internal("failed to open Stockfish stdout".into()))?;

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(read_lines_loop(stdout, tx));

        send_line(&mut stdin, "uci").await?;
        let name = wait_for_uciok(&mut rx, ENGINE_TIMEOUT, &self.path).await?;

        let mut applied = default_uci_options();
        for (k, v) in &applied {
            send_line(&mut stdin, &format!("setoption name {} value {}", k, v)).await?;
        }
        applied.insert("MultiPV".to_string(), DEFAULT_MULTIPV.to_string());
        send_line(
            &mut stdin,
            &format!("setoption name MultiPV value {}", DEFAULT_MULTIPV),
        )
        .await?;

        send_line(&mut stdin, "isready").await?;
        wait_for(&mut rx, |l| l == "readyok", ENGINE_TIMEOUT)
            .await
            .map_err(|_| ChessError::EngineUnavailable {
                message: "engine did not respond with readyok".into(),
                expected_path: self.path.display().to_string(),
            })?;

        Ok(Spawned {
            child,
            stdin,
            name,
            uci_options: applied,
            reader_rx: rx,
        })
    }
}

async fn read_lines_loop(stdout: ChildStdout, tx: mpsc::UnboundedSender<String>) {
    let mut reader = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if tx.send(line).is_err() {
            break;
        }
    }
}

async fn send_line(stdin: &mut ChildStdin, line: &str) -> Result<(), ChessError> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    stdin
        .write_all(buf.as_bytes())
        .await
        .map_err(|e| ChessError::Internal(format!("failed writing to engine: {}", e)))?;
    stdin
        .flush()
        .await
        .map_err(|e| ChessError::Internal(format!("failed flushing engine stdin: {}", e)))?;
    Ok(())
}

async fn recv_with_timeout(
    rx: &mut mpsc::UnboundedReceiver<String>,
    timeout: Duration,
) -> Result<Option<String>, tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, rx.recv()).await
}

async fn wait_for<F>(
    rx: &mut mpsc::UnboundedReceiver<String>,
    pred: F,
    timeout: Duration,
) -> Result<String, ()>
where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(line)) => {
                if pred(&line) {
                    return Ok(line);
                }
            }
            Ok(None) => return Err(()),
            Err(_) => return Err(()),
        }
    }
}

async fn wait_for_uciok(
    rx: &mut mpsc::UnboundedReceiver<String>,
    timeout: Duration,
    expected_path: &Path,
) -> Result<String, ChessError> {
    let mut name = "Stockfish".to_string();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(ChessError::EngineUnavailable {
                message: "did not receive uciok in time".into(),
                expected_path: expected_path.display().to_string(),
            });
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(line)) => {
                if let Some(rest) = line.strip_prefix("id name ") {
                    name = rest.trim().to_string();
                } else if line.trim() == "uciok" {
                    return Ok(name);
                }
            }
            Ok(None) | Err(_) => {
                return Err(ChessError::EngineUnavailable {
                    message: "engine closed before uciok".into(),
                    expected_path: expected_path.display().to_string(),
                });
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParsedInfo {
    pub depth: u32,
    pub multipv: u32,
    pub line: AnalysisLine,
}

/// Parse a Stockfish `info` line. Returns None if it lacks the fields we care about.
pub fn parse_info_line(line: &str) -> Option<ParsedInfo> {
    let line = line.trim();
    if !line.starts_with("info ") {
        return None;
    }
    let mut tokens = line.split_ascii_whitespace().peekable();
    tokens.next(); // "info"

    let mut depth: Option<u32> = None;
    let mut multipv: u32 = 1;
    let mut score_cp: Option<i32> = None;
    let mut score_mate: Option<i32> = None;
    let mut pv: Vec<String> = Vec::new();

    while let Some(tok) = tokens.next() {
        match tok {
            "depth" => {
                if let Some(v) = tokens.next() {
                    depth = v.parse().ok();
                }
            }
            "multipv" => {
                if let Some(v) = tokens.next() {
                    multipv = v.parse().unwrap_or(1);
                }
            }
            "score" => {
                if let Some(kind) = tokens.next() {
                    if let Some(val) = tokens.next() {
                        match kind {
                            "cp" => score_cp = val.parse().ok(),
                            "mate" => score_mate = val.parse().ok(),
                            _ => {}
                        }
                    }
                }
            }
            "pv" => {
                // Per UCI spec, pv is always the last field on the line.
                for m in tokens.by_ref() {
                    pv.push(m.to_string());
                }
            }
            _ => {}
        }
    }

    let depth = depth?;
    if pv.is_empty() && score_cp.is_none() && score_mate.is_none() {
        return None;
    }
    let score = if let Some(mate) = score_mate {
        Score {
            kind: ScoreKind::Mate,
            cp: None,
            mate_in: Some(mate),
        }
    } else {
        Score {
            kind: ScoreKind::Cp,
            cp: score_cp,
            mate_in: None,
        }
    };
    Some(ParsedInfo {
        depth,
        multipv,
        line: AnalysisLine {
            rank: multipv,
            score,
            pv_uci: pv,
            pv_san: Vec::new(),
            captures: Vec::new(),
            material_swing: Default::default(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_contain_required_options() {
        let opts = default_uci_options();
        assert_eq!(opts.get("Threads").map(String::as_str), Some("3"));
        assert_eq!(opts.get("Hash").map(String::as_str), Some("128"));
        assert_eq!(opts.get("UCI_AnalyseMode").map(String::as_str), Some("true"));
        assert_eq!(opts.get("Contempt").map(String::as_str), Some("0"));
    }

    #[test]
    fn parse_basic_cp_info_line() {
        let line = "info depth 12 seldepth 18 multipv 1 score cp 31 nodes 100 nps 50 pv e2e4 e7e5 g1f3";
        let parsed = parse_info_line(line).unwrap();
        assert_eq!(parsed.depth, 12);
        assert_eq!(parsed.multipv, 1);
        assert_eq!(parsed.line.rank, 1);
        assert_eq!(parsed.line.score.kind, ScoreKind::Cp);
        assert_eq!(parsed.line.score.cp, Some(31));
        assert_eq!(parsed.line.score.mate_in, None);
        assert_eq!(parsed.line.pv_uci, vec!["e2e4", "e7e5", "g1f3"]);
    }

    #[test]
    fn parse_mate_info_line() {
        let line = "info depth 10 multipv 2 score mate 3 pv h5f7 e8e7 f7f5";
        let parsed = parse_info_line(line).unwrap();
        assert_eq!(parsed.depth, 10);
        assert_eq!(parsed.multipv, 2);
        assert_eq!(parsed.line.score.kind, ScoreKind::Mate);
        assert_eq!(parsed.line.score.mate_in, Some(3));
        assert_eq!(parsed.line.score.cp, None);
    }

    #[test]
    fn parse_info_line_missing_pv_and_score_returns_none() {
        let line = "info depth 5 nodes 1000";
        assert!(parse_info_line(line).is_none());
    }

    #[test]
    fn parse_non_info_line_returns_none() {
        assert!(parse_info_line("bestmove e2e4").is_none());
        assert!(parse_info_line("readyok").is_none());
        assert!(parse_info_line("").is_none());
    }

    #[test]
    fn parse_multipv_default_one_when_missing() {
        let line = "info depth 3 score cp 10 pv e2e4";
        let parsed = parse_info_line(line).unwrap();
        assert_eq!(parsed.multipv, 1);
        assert_eq!(parsed.line.rank, 1);
    }

    #[test]
    fn parse_negative_cp_score() {
        let line = "info depth 8 multipv 1 score cp -42 pv d7d5";
        let parsed = parse_info_line(line).unwrap();
        assert_eq!(parsed.line.score.cp, Some(-42));
    }

    #[test]
    fn engine_unavailable_when_path_missing() {
        let engine = Arc::new(Engine::new("/definitely/not/here/stockfish"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(engine.status()).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::EngineUnavailable);
    }
}
