use std::env;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Deserialize;

pub const ENV_STOCKFISH_PATH: &str = "CHESS_MCP_STOCKFISH_PATH";
pub const ENV_CACHE_CAPACITY: &str = "CHESS_MCP_CACHE_CAPACITY";
pub const ENV_LOG_LEVEL: &str = "CHESS_MCP_LOG_LEVEL";
pub const DEFAULT_CONFIG_FILE: &str = "chess-mcp.toml";
pub const DEFAULT_CACHE_CAPACITY: usize = 1024;
pub const DEFAULT_LOG_LEVEL: &str = "info";

/// CLI arguments for the chess-pos-analyzer MCP server.
#[derive(Debug, Parser, Default, Clone)]
#[command(name = "chess-pos-analyzer", about = "Local MCP server for chess position analysis")]
pub struct CliArgs {
    /// Path to the Stockfish executable.
    #[arg(long = "stockfish-path", value_name = "PATH")]
    pub stockfish_path: Option<PathBuf>,

    /// Maximum number of analysis cache entries.
    #[arg(long = "cache-capacity", value_name = "N")]
    pub cache_capacity: Option<usize>,

    /// Log level (trace, debug, info, warn, error).
    #[arg(long = "log-level", value_name = "LEVEL")]
    pub log_level: Option<String>,

    /// Path to the configuration file (default: ./chess-mcp.toml).
    #[arg(long = "config", value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub stockfish_path: Option<PathBuf>,
    pub cache_capacity: Option<usize>,
    pub log_level: Option<String>,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path:?}: {message}")]
    Parse { path: PathBuf, message: String },
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub stockfish_path: PathBuf,
    pub cache_capacity: usize,
    pub log_level: String,
}

impl ResolvedConfig {
    /// Resolve configuration from CLI > env > file > platform default.
    pub fn resolve(cli: CliArgs) -> Result<Self, ConfigError> {
        let config_path = cli
            .config
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_FILE));
        let file_config = FileConfig::load(&config_path)?;

        let stockfish_path = cli
            .stockfish_path
            .or_else(|| env::var(ENV_STOCKFISH_PATH).ok().map(PathBuf::from))
            .or(file_config.stockfish_path)
            .unwrap_or_else(platform_default_stockfish_path);

        let cache_capacity = cli
            .cache_capacity
            .or_else(|| {
                env::var(ENV_CACHE_CAPACITY)
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .or(file_config.cache_capacity)
            .unwrap_or(DEFAULT_CACHE_CAPACITY)
            .max(1);

        let log_level = cli
            .log_level
            .or_else(|| env::var(ENV_LOG_LEVEL).ok())
            .or(file_config.log_level)
            .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string());

        Ok(Self {
            stockfish_path,
            cache_capacity,
            log_level,
        })
    }
}

pub fn platform_default_stockfish_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("stockfish/stockfish.exe")
    } else {
        PathBuf::from("stockfish/stockfish")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialize env-touching tests. Cargo runs tests in parallel by default and
    // env vars are process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        env::remove_var(ENV_STOCKFISH_PATH);
        env::remove_var(ENV_CACHE_CAPACITY);
        env::remove_var(ENV_LOG_LEVEL);
    }

    #[test]
    fn cli_overrides_env_and_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        env::set_var(ENV_STOCKFISH_PATH, "/from/env/sf");
        let cli = CliArgs {
            stockfish_path: Some(PathBuf::from("/cli/sf")),
            cache_capacity: Some(42),
            log_level: Some("trace".into()),
            config: Some(PathBuf::from("/nonexistent/chess-mcp.toml")),
        };
        let cfg = ResolvedConfig::resolve(cli).unwrap();
        assert_eq!(cfg.stockfish_path, PathBuf::from("/cli/sf"));
        assert_eq!(cfg.cache_capacity, 42);
        assert_eq!(cfg.log_level, "trace");
        clear_env();
    }

    #[test]
    fn env_overrides_file_when_cli_missing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        env::set_var(ENV_STOCKFISH_PATH, "/from/env/sf");
        env::set_var(ENV_CACHE_CAPACITY, "7");
        let cli = CliArgs {
            config: Some(PathBuf::from("/nonexistent.toml")),
            ..Default::default()
        };
        let cfg = ResolvedConfig::resolve(cli).unwrap();
        assert_eq!(cfg.stockfish_path, PathBuf::from("/from/env/sf"));
        assert_eq!(cfg.cache_capacity, 7);
        clear_env();
    }

    #[test]
    fn file_used_when_cli_and_env_absent() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("chess-mcp.toml");
        std::fs::write(
            &path,
            "stockfish_path = \"/file/sf\"\ncache_capacity = 99\nlog_level = \"warn\"\n",
        )
        .unwrap();
        let cli = CliArgs {
            config: Some(path),
            ..Default::default()
        };
        let cfg = ResolvedConfig::resolve(cli).unwrap();
        assert_eq!(cfg.stockfish_path, PathBuf::from("/file/sf"));
        assert_eq!(cfg.cache_capacity, 99);
        assert_eq!(cfg.log_level, "warn");
        clear_env();
    }

    #[test]
    fn defaults_when_nothing_supplied() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        let cli = CliArgs {
            config: Some(PathBuf::from("/nonexistent.toml")),
            ..Default::default()
        };
        let cfg = ResolvedConfig::resolve(cli).unwrap();
        assert_eq!(cfg.stockfish_path, platform_default_stockfish_path());
        assert_eq!(cfg.cache_capacity, DEFAULT_CACHE_CAPACITY);
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL);
        clear_env();
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("chess-mcp.toml");
        std::fs::write(&path, "this is not = valid toml [").unwrap();
        let err = FileConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn cache_capacity_is_at_least_one() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        let cli = CliArgs {
            cache_capacity: Some(0),
            config: Some(PathBuf::from("/nonexistent.toml")),
            ..Default::default()
        };
        let cfg = ResolvedConfig::resolve(cli).unwrap();
        assert_eq!(cfg.cache_capacity, 1);
    }
}
