//! Persistent per-key health plus the round-robin cursor.
//!
//! The state lives in a JSON file. Every read-modify-write goes through
//! [`StateStore::update`], which takes an exclusive advisory lock on a
//! sibling `.lock` file so concurrent CLI invocations cannot clobber each
//! other or hand out the same key.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::write_private;
use crate::error::{Error, Result};

/// Schema version written into the file.
pub const STATE_VERSION: u32 = 1;

/// Lifecycle of a key as far as the pool is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    /// Usable (possibly cooling down).
    #[default]
    Active,
    /// Exa returned 402: no credits or budget left. Cleared by `keys reset`.
    Exhausted,
    /// Exa returned 401: key rejected. Cleared by `keys reset`.
    Invalid,
}

/// Health record for a single key, addressed by its fingerprint.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyState {
    /// Current lifecycle status.
    pub status: KeyStatus,
    /// Unix milliseconds until which the key must not be used.
    pub cooldown_until_ms: Option<u64>,
    /// Transient failures in a row (reset on success or quarantine).
    pub consecutive_failures: u32,
    /// When `status` last changed, unix milliseconds.
    pub status_changed_at_ms: Option<u64>,
    /// Last time the key was handed out, unix milliseconds.
    pub last_used_at_ms: Option<u64>,
    /// Most recent error text, if any.
    pub last_error: Option<String>,
    /// Successful requests served.
    pub ok_requests: u64,
    /// Sum of `costDollars.total` over successful responses.
    pub spent_usd: f64,
}

impl KeyState {
    /// Whether the key may be handed out at `now_ms`.
    #[must_use]
    pub fn is_eligible(&self, now_ms: u64) -> bool {
        self.status == KeyStatus::Active && self.cooldown_until_ms.is_none_or(|t| t <= now_ms)
    }
}

/// Whole-file contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Schema version.
    pub version: u32,
    /// Index of the next key to try (modulo pool size).
    pub cursor: u64,
    /// Per-key health keyed by fingerprint.
    pub keys: BTreeMap<String, KeyState>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            cursor: 0,
            keys: BTreeMap::new(),
        }
    }
}

/// Handle to the on-disk state file.
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// Point at `path` (created lazily on first update).
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Location of the state file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the state without locking. Missing file yields the default.
    ///
    /// # Errors
    /// When the file exists but is unreadable or malformed.
    pub fn load(&self) -> Result<State> {
        read_state(&self.path)
    }

    /// Lock, load, apply `f`, persist atomically, unlock.
    ///
    /// # Errors
    /// When the directory, lock file, or state file cannot be accessed.
    pub fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| Error::State(format!("{} has no parent", self.path.display())))?;
        fs::create_dir_all(dir).map_err(|e| Error::State(format!("{}: {e}", dir.display())))?;
        let _guard = LockGuard::acquire(&self.path.with_extension("lock"))?;
        let mut state = read_state(&self.path)?;
        let out = f(&mut state);
        let text = serde_json::to_string_pretty(&state).map_err(|e| Error::State(e.to_string()))?;
        write_private(&self.path, text.as_bytes())
            .map_err(|e| Error::State(format!("{}: {e}", self.path.display())))?;
        Ok(out)
    }
}

fn read_state(path: &Path) -> Result<State> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| Error::State(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(Error::State(format!("{}: {e}", path.display()))),
    }
}

/// Exclusive advisory lock released on drop.
struct LockGuard {
    file: File,
}

impl LockGuard {
    fn acquire(path: &Path) -> Result<Self> {
        let file = File::create(path)
            .map_err(|e| Error::State(format!("lock {}: {e}", path.display())))?;
        file.lock()
            .map_err(|e| Error::State(format!("lock {}: {e}", path.display())))?;
        Ok(Self { file })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock anyway; explicit unlock keeps intent clear.
        let _ = self.file.unlock();
    }
}

/// Stable, non-secret identifier for a key (FNV-1a 64 as hex).
#[must_use]
pub fn fingerprint(key: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = key
        .bytes()
        .fold(OFFSET, |h, b| (h ^ u64::from(b)).wrapping_mul(PRIME));
    format!("{hash:016x}")
}

/// Human-friendly, non-secret label: first and last four characters.
#[must_use]
pub fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_distinct() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("").len(), 16);
    }

    #[test]
    fn mask_hides_middle() {
        assert_eq!(mask("12345678-abcd-efgh-ijkl-mnopqrstuvwx"), "1234…uvwx");
        assert_eq!(mask("short"), "*****");
    }

    #[test]
    fn eligibility_respects_status_and_cooldown() {
        let mut ks = KeyState::default();
        assert!(ks.is_eligible(0));
        ks.cooldown_until_ms = Some(100);
        assert!(!ks.is_eligible(50));
        assert!(ks.is_eligible(100));
        ks.status = KeyStatus::Exhausted;
        assert!(!ks.is_eligible(1_000));
    }

    #[test]
    fn update_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("deep/state.json"));
        assert_eq!(store.load().unwrap(), State::default());
        let cursor = store
            .update(|s| {
                s.cursor = 7;
                s.keys.entry("k".into()).or_default().ok_requests = 2;
                s.cursor
            })
            .unwrap();
        assert_eq!(cursor, 7);
        let again = store.load().unwrap();
        assert_eq!(again.cursor, 7);
        assert_eq!(again.keys["k"].ok_requests, 2);
        assert!(!dir.path().join("deep/state.tmp").exists());
    }

    #[test]
    fn malformed_state_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, "{ not json").unwrap();
        let err = StateStore::new(path).load().unwrap_err();
        assert!(matches!(err, Error::State(_)));
    }
}
