//! Configuration file, environment overrides, and on-disk locations.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Environment variable holding a comma/newline separated list of keys.
pub const ENV_KEYS: &str = "EXA_API_KEYS";
/// Environment variable overriding the config/state directory.
pub const ENV_HOME: &str = "EXA_POOL_HOME";

/// Tunables persisted in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Exa API origin.
    pub base_url: String,
    /// Per-request timeout.
    pub timeout_seconds: u64,
    /// Attempt budget per CLI invocation (across all keys).
    pub max_attempts: u32,
    /// Consecutive transient failures before a key is quarantined.
    pub max_consecutive_failures: u32,
    /// Quarantine length after too many transient failures.
    pub quarantine_seconds: u64,
    /// Cooldown applied on HTTP 429 when Exa sends no `Retry-After`.
    pub rate_limit_cooldown_ms: u64,
    /// Longest the CLI will sleep waiting for a cooling key before giving up.
    pub max_wait_ms: u64,
    /// API keys, tried in order, round-robin across invocations.
    pub keys: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "https://api.exa.ai".into(),
            timeout_seconds: 90,
            max_attempts: 6,
            max_consecutive_failures: 3,
            quarantine_seconds: 300,
            rate_limit_cooldown_ms: 1_000,
            max_wait_ms: 10_000,
            keys: Vec::new(),
        }
    }
}

/// Where the CLI keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Directory containing everything below.
    pub home: PathBuf,
    /// `config.toml`.
    pub config: PathBuf,
    /// `state.json`.
    pub state: PathBuf,
}

impl Paths {
    /// Resolve the home directory: explicit flag, then `EXA_POOL_HOME`,
    /// then `$XDG_CONFIG_HOME/exa-pool`, then `~/.config/exa-pool` where
    /// `~` is `HOME`, or `USERPROFILE` on Windows.
    ///
    /// # Errors
    /// When no candidate directory can be derived from the environment.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let home = explicit
            .or_else(|| env::var_os(ENV_HOME).map(PathBuf::from))
            .or_else(|| env::var_os("XDG_CONFIG_HOME").map(|d| PathBuf::from(d).join("exa-pool")))
            .or_else(|| {
                env::var_os("HOME")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .map(|h| PathBuf::from(h).join(".config/exa-pool"))
            })
            .ok_or_else(|| Error::Config("cannot locate home directory".into()))?;
        Ok(Self::in_dir(home))
    }

    /// Build paths rooted at `home` without consulting the environment.
    #[must_use]
    pub fn in_dir(home: PathBuf) -> Self {
        let config = home.join("config.toml");
        let state = home.join("state.json");
        Self {
            home,
            config,
            state,
        }
    }
}

/// Load `config.toml` if present and merge keys from the environment.
///
/// Key precedence: file keys first, then `EXA_API_KEYS`. The single-key
/// `EXA_API_KEY` variable is deliberately ignored so a key exported for other
/// tools never joins the pool by accident.
/// Duplicates and blanks are dropped while preserving first-seen order.
///
/// # Errors
/// When the file exists but cannot be read or parsed.
pub fn load(paths: &Paths) -> Result<Config> {
    let mut config = load_file(&paths.config)?;
    let env_keys = env::var(ENV_KEYS)
        .ok()
        .map(|v| split_keys(&v))
        .unwrap_or_default();
    let merged = config.keys.iter().cloned().chain(env_keys);
    config.keys = dedup(merged);
    Ok(config)
}

/// Read the file portion only (no environment merge).
///
/// # Errors
/// When the file exists but cannot be read or parsed.
pub fn load_file(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(Error::Config(format!("{}: {e}", path.display()))),
    }
}

/// Write the config file with owner-only permissions.
///
/// # Errors
/// When the directory cannot be created or the file cannot be written.
pub fn save(paths: &Paths, config: &Config) -> Result<()> {
    fs::create_dir_all(&paths.home)
        .map_err(|e| Error::Config(format!("{}: {e}", paths.home.display())))?;
    let text = toml::to_string_pretty(config).map_err(|e| Error::Config(e.to_string()))?;
    write_private(&paths.config, text.as_bytes())
        .map_err(|e| Error::Config(format!("{}: {e}", paths.config.display())))
}

/// Split an environment value on commas, whitespace, or newlines.
#[must_use]
pub fn split_keys(raw: &str) -> Vec<String> {
    raw.split([',', '\n', ' ', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dedup(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    keys.filter(|k| !k.is_empty())
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

/// Write `bytes` to `path` atomically (temp file + rename) with mode `0600`.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_handles_mixed_separators() {
        assert_eq!(split_keys(" a, b\nc\t d ,, "), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn dedup_keeps_first_order() {
        let out = dedup(["b", "a", "b", "", "c", "a"].into_iter().map(String::from));
        assert_eq!(out, vec!["b", "a", "c"]);
    }

    #[test]
    fn roundtrip_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path().join("nested"));
        let cfg = Config {
            keys: vec!["k1".into(), "k2".into()],
            max_attempts: 9,
            ..Config::default()
        };
        save(&paths, &cfg).unwrap();
        assert_eq!(load_file(&paths.config).unwrap(), cfg);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path().to_path_buf());
        assert_eq!(load_file(&paths.config).unwrap(), Config::default());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = toml::from_str::<Config>("bogus = 1").unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }
}
