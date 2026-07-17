//! Config precedence: env > `~/.config/receipts/config.toml` > built-in defaults.
//! Missing API keys are not a load-time error — `doctor` reports them, and
//! `ask` fails with an Auth error only when a key is actually needed.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ReceiptsError;

pub const DEFAULT_MODEL: &str = "gemma-4-31b";
pub const DEFAULT_API_BASE: &str = "https://api.cerebras.ai/v1";
pub const DEFAULT_MAX_CONCURRENCY: u32 = 25;
/// Exa search type: A/B measured 2026-07-02 end-to-end (3 questions, depth
/// standard). fast: 5.3s mean, 19.3 supported+partial claims. instant: 9.3s,
/// 18.7 — its thinner results push workers into extra search rounds, negating
/// the raw ~370ms-vs-575ms per-search edge. auto: 10.3s, 13.3.
pub const DEFAULT_EXA_SEARCH_TYPE: &str = "fast";
pub const EXA_SEARCH_TYPES: &[&str] = &["fast", "instant", "auto"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cerebras_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub model: String,
    pub api_base: String,
    pub max_concurrency: u32,
    pub exa_search_type: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cerebras_api_key: None,
            exa_api_key: None,
            model: DEFAULT_MODEL.to_string(),
            api_base: DEFAULT_API_BASE.to_string(),
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            exa_search_type: DEFAULT_EXA_SEARCH_TYPE.to_string(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    cerebras_api_key: Option<String>,
    exa_api_key: Option<String>,
    model: Option<String>,
    api_base: Option<String>,
    max_concurrency: Option<u32>,
    exa_search_type: Option<String>,
}

fn default_config_path() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/receipts/config.toml"))
}

fn read_file_config(path: &Path) -> Result<FileConfig, ReceiptsError> {
    let text = fs::read_to_string(path).map_err(|e| {
        ReceiptsError::config(format!(
            "failed to read config file {}: {e}",
            path.display()
        ))
    })?;
    toml::from_str(&text).map_err(|e| {
        ReceiptsError::config(format!(
            "failed to parse config file {}: {e}",
            path.display()
        ))
    })
}

fn env_over_file(key: &str, file_value: Option<String>) -> Option<String> {
    env::var(key).ok().or(file_value)
}

impl Config {
    /// Loads config from `~/.config/receipts/config.toml` (if present) merged
    /// under environment variables, falling back to built-in defaults.
    pub fn load() -> Result<Self, ReceiptsError> {
        Self::load_from(default_config_path().as_deref())
    }

    /// Same as `load`, but with an explicit (possibly absent) config file
    /// path — the seam tests use to avoid touching the real home directory.
    fn load_from(path: Option<&Path>) -> Result<Self, ReceiptsError> {
        let file_cfg = match path {
            Some(p) if p.exists() => read_file_config(p)?,
            _ => FileConfig::default(),
        };

        let defaults = Config::default();

        let exa_search_type = env_over_file("RECEIPTS_EXA_SEARCH_TYPE", file_cfg.exa_search_type)
            .unwrap_or(defaults.exa_search_type);
        if !EXA_SEARCH_TYPES.contains(&exa_search_type.as_str()) {
            return Err(ReceiptsError::config(format!(
                "invalid exa search type {exa_search_type:?}; expected one of: {}",
                EXA_SEARCH_TYPES.join(", ")
            )));
        }

        Ok(Config {
            cerebras_api_key: env_over_file("CEREBRAS_API_KEY", file_cfg.cerebras_api_key),
            exa_api_key: env_over_file("EXA_API_KEY", file_cfg.exa_api_key),
            model: env_over_file("RECEIPTS_MODEL", file_cfg.model).unwrap_or(defaults.model),
            api_base: env_over_file("RECEIPTS_API_BASE", file_cfg.api_base)
                .unwrap_or(defaults.api_base),
            max_concurrency: env::var("RECEIPTS_MAX_CONCURRENCY")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .or(file_cfg.max_concurrency)
                .unwrap_or(defaults.max_concurrency),
            exa_search_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// `Config::load` reads process-wide env vars; serialize the tests that
    /// touch them so parallel `cargo test` runs don't race each other.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    const ENV_KEYS: &[&str] = &[
        "CEREBRAS_API_KEY",
        "EXA_API_KEY",
        "RECEIPTS_MODEL",
        "RECEIPTS_API_BASE",
        "RECEIPTS_MAX_CONCURRENCY",
        "RECEIPTS_EXA_SEARCH_TYPE",
    ];

    fn clear_env() {
        for key in ENV_KEYS {
            // SAFETY: serialized by `env_lock`; no other thread reads/writes
            // these process-wide env vars concurrently.
            unsafe { env::remove_var(key) };
        }
    }

    fn set_env(key: &str, value: &str) {
        // SAFETY: serialized by `env_lock`; no other thread reads/writes
        // these process-wide env vars concurrently.
        unsafe { env::set_var(key, value) };
    }

    #[test]
    fn defaults_used_when_nothing_set() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let cfg = Config::load_from(None).unwrap();

        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert_eq!(cfg.api_base, DEFAULT_API_BASE);
        assert_eq!(cfg.max_concurrency, DEFAULT_MAX_CONCURRENCY);
    }

    #[test]
    fn env_override_wins_over_defaults() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        set_env("CEREBRAS_API_KEY", "env-cerebras-key");
        set_env("RECEIPTS_MODEL", "some-other-model");
        set_env("RECEIPTS_MAX_CONCURRENCY", "7");

        let cfg = Config::load_from(None).unwrap();

        assert_eq!(cfg.cerebras_api_key.as_deref(), Some("env-cerebras-key"));
        assert_eq!(cfg.model, "some-other-model");
        assert_eq!(cfg.max_concurrency, 7);
        assert_eq!(cfg.api_base, DEFAULT_API_BASE);

        clear_env();
    }

    #[test]
    fn file_config_used_when_no_env_set() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let dir = std::env::temp_dir().join(format!("receipts-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(
            &path,
            r#"
            exa_api_key = "file-exa-key"
            model = "file-model"
            max_concurrency = 3
            "#,
        )
        .unwrap();

        let cfg = Config::load_from(Some(&path)).unwrap();

        assert_eq!(cfg.exa_api_key.as_deref(), Some("file-exa-key"));
        assert_eq!(cfg.model, "file-model");
        assert_eq!(cfg.max_concurrency, 3);
        assert_eq!(cfg.cerebras_api_key, None);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_wins_over_file() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let dir = std::env::temp_dir().join(format!("receipts-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, r#"model = "file-model""#).unwrap();

        set_env("RECEIPTS_MODEL", "env-model");

        let cfg = Config::load_from(Some(&path)).unwrap();
        assert_eq!(cfg.model, "env-model");

        clear_env();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_api_keys_are_not_a_load_error() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let result = Config::load_from(None);
        assert!(result.is_ok());
        let cfg = result.unwrap();
        assert_eq!(cfg.cerebras_api_key, None);
        assert_eq!(cfg.exa_api_key, None);
    }

    #[test]
    fn exa_search_type_defaults_to_fast_and_env_overrides() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let cfg = Config::load_from(None).unwrap();
        assert_eq!(cfg.exa_search_type, "fast");

        set_env("RECEIPTS_EXA_SEARCH_TYPE", "instant");
        let cfg = Config::load_from(None).unwrap();
        assert_eq!(cfg.exa_search_type, "instant");

        clear_env();
    }

    #[test]
    fn invalid_exa_search_type_is_a_config_error() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        set_env("RECEIPTS_EXA_SEARCH_TYPE", "warp-speed");
        let err = Config::load_from(None).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("warp-speed"));

        clear_env();
    }

    #[test]
    fn malformed_config_file_is_a_config_error() {
        let _guard = env_lock().lock().unwrap();
        clear_env();

        let dir = std::env::temp_dir().join(format!("receipts-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "not valid toml [[[").unwrap();

        let err = Config::load_from(Some(&path)).unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.code(), "config");

        fs::remove_dir_all(&dir).unwrap();
    }
}
