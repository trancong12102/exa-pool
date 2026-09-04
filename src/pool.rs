//! Key selection, request execution, and verdict application.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::exa::Request;
use crate::policy::{Verdict, classify, classify_transport};
use crate::state::{KeyState, KeyStatus, State, StateStore, fingerprint, mask};
use crate::transport::{Method, Transport};

/// Cap on the exponential backoff between transient retries.
const MAX_BACKOFF_MS: u64 = 4_000;
/// Base of the exponential backoff.
const BASE_BACKOFF_MS: u64 = 250;

/// Time source, injectable for tests.
pub trait Clock {
    /// Unix time in milliseconds.
    fn now_ms(&self) -> u64;
    /// Block for `d`.
    fn sleep(&self, d: Duration);
}

/// Wall clock plus real sleeping.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// A configured key with its stable identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    /// The secret.
    pub key: String,
    /// Fingerprint used in the state file.
    pub id: String,
    /// Masked label for logs.
    pub label: String,
}

impl KeyEntry {
    /// Derive identifiers from a raw key.
    #[must_use]
    pub fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            id: fingerprint(key),
            label: mask(key),
        }
    }
}

/// Callback receiving [`Event`]s.
pub type Observer<'a> = &'a dyn Fn(&Event);

/// Diagnostics emitted while a request is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// One attempt finished.
    Attempt {
        /// Attempt number starting at 1.
        attempt: u32,
        /// Masked key label.
        label: String,
        /// Verdict summary.
        summary: String,
    },
    /// Every eligible key is cooling down; sleeping before retrying.
    Waiting {
        /// Sleep length.
        ms: u64,
    },
    /// Backing off after a transient failure.
    Backoff {
        /// Sleep length.
        ms: u64,
    },
}

/// Result of asking the state for a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Use this index into the key list.
    Key(usize),
    /// All remaining keys are cooling; earliest becomes available after this long.
    Wait(Duration),
    /// Nothing can ever be used without operator action.
    None(String),
}

/// Pick the next eligible key round-robin and advance the cursor.
///
/// Keys that are exhausted or invalid are skipped. Keys that are cooling
/// down are skipped but remembered so the caller can wait for the earliest.
pub fn select(state: &mut State, keys: &[KeyEntry], now_ms: u64) -> Selection {
    if keys.is_empty() {
        return Selection::None("no api keys configured".into());
    }
    let len = keys.len();
    let start = wrap(usize::try_from(state.cursor).unwrap_or(0), len);
    let mut earliest: Option<u64> = None;
    let mut blocked = (0_usize, 0_usize);

    for offset in 0..len {
        let idx = wrap(start.saturating_add(offset), len);
        let Some(entry) = keys.get(idx) else { continue };
        let ks = state.keys.get(&entry.id).cloned().unwrap_or_default();
        match ks.status {
            KeyStatus::Exhausted => blocked.0 = blocked.0.saturating_add(1),
            KeyStatus::Invalid => blocked.1 = blocked.1.saturating_add(1),
            KeyStatus::Active => match ks.cooldown_until_ms {
                Some(until) if until > now_ms => {
                    earliest = Some(earliest.map_or(until, |e| e.min(until)));
                }
                _ => {
                    state.cursor = u64::try_from(wrap(idx.saturating_add(1), len)).unwrap_or(0);
                    return Selection::Key(idx);
                }
            },
        }
    }

    earliest.map_or_else(
        || {
            Selection::None(format!(
                "all {len} key(s) unusable: {} exhausted, {} invalid (run `exa-search keys reset` after topping up)",
                blocked.0, blocked.1
            ))
        },
        |until| Selection::Wait(Duration::from_millis(until.saturating_sub(now_ms))),
    )
}

/// `i % len` without the operator lint; `len == 0` yields 0.
const fn wrap(i: usize, len: usize) -> usize {
    match i.checked_rem(len) {
        Some(r) => r,
        None => 0,
    }
}

/// Record the outcome of an attempt against `id`.
pub fn apply(state: &mut State, id: &str, verdict: &Verdict, now_ms: u64, config: &Config) {
    let ks: &mut KeyState = state.keys.entry(id.to_owned()).or_default();
    ks.last_used_at_ms = Some(now_ms);
    match verdict {
        Verdict::Success { cost, .. } => {
            ks.consecutive_failures = 0;
            ks.cooldown_until_ms = None;
            ks.last_error = None;
            ks.ok_requests = ks.ok_requests.saturating_add(1);
            ks.spent_usd += cost;
        }
        Verdict::KeyExhausted { tag, message } => {
            ks.status = KeyStatus::Exhausted;
            ks.status_changed_at_ms = Some(now_ms);
            ks.last_error = Some(format!("402 {tag}: {message}"));
        }
        Verdict::KeyInvalid { message } => {
            ks.status = KeyStatus::Invalid;
            ks.status_changed_at_ms = Some(now_ms);
            ks.last_error = Some(format!("401 INVALID_API_KEY: {message}"));
        }
        Verdict::RateLimited {
            retry_after,
            message,
        } => {
            let wait = retry_after.map_or(config.rate_limit_cooldown_ms, |d| {
                u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
            });
            ks.cooldown_until_ms = Some(now_ms.saturating_add(wait));
            ks.last_error = Some(format!("429: {message}"));
        }
        Verdict::Transient { message } => {
            ks.consecutive_failures = ks.consecutive_failures.saturating_add(1);
            ks.last_error = Some(message.clone());
            if ks.consecutive_failures >= config.max_consecutive_failures {
                ks.consecutive_failures = 0;
                ks.cooldown_until_ms =
                    Some(now_ms.saturating_add(config.quarantine_seconds.saturating_mul(1_000)));
            }
        }
        Verdict::RequestError { .. } => {
            // Not the key's fault; leave its health untouched.
        }
    }
}

/// Executes requests against the pool.
pub struct Pool<'a, T: Transport, C: Clock> {
    config: &'a Config,
    keys: Vec<KeyEntry>,
    store: &'a StateStore,
    transport: &'a T,
    clock: &'a C,
    observer: Option<Observer<'a>>,
}

impl<T: Transport, C: Clock> std::fmt::Debug for Pool<'_, T, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("keys", &self.keys.len())
            .field("state", &self.store.path())
            .finish_non_exhaustive()
    }
}

impl<'a, T: Transport, C: Clock> Pool<'a, T, C> {
    /// Assemble a pool from config keys.
    #[must_use]
    pub fn new(config: &'a Config, store: &'a StateStore, transport: &'a T, clock: &'a C) -> Self {
        let keys = config.keys.iter().map(|k| KeyEntry::new(k)).collect();
        Self {
            config,
            keys,
            store,
            transport,
            clock,
            observer: None,
        }
    }

    /// Receive [`Event`]s as they happen.
    #[must_use]
    pub fn with_observer(mut self, observer: Observer<'a>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn emit(&self, event: &Event) {
        if let Some(obs) = self.observer {
            obs(event);
        }
    }

    /// Run `request`, rotating keys per the policy, and return the raw body.
    ///
    /// # Errors
    /// - [`Error::Request`] when Exa rejects the request itself.
    /// - [`Error::NoUsableKeys`] when nothing in the pool can serve it.
    /// - [`Error::Upstream`] when transient failures exceed `max_attempts`.
    /// - [`Error::State`] when the state file cannot be updated.
    pub fn execute(&self, request: &Request) -> Result<String> {
        let url = request.url(&self.config.base_url);
        let mut attempts: u32 = 0;
        let mut last_error = String::from("no attempt made");

        loop {
            if attempts >= self.config.max_attempts {
                return Err(Error::Upstream {
                    attempts,
                    last: last_error,
                });
            }
            let now = self.clock.now_ms();
            let selection = self.store.update(|s| select(s, &self.keys, now))?;
            let idx = match selection {
                Selection::Key(idx) => idx,
                Selection::Wait(d) => {
                    let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
                    if ms > self.config.max_wait_ms {
                        return Err(Error::NoUsableKeys(format!(
                            "every key is cooling down; earliest frees in {ms}ms (max_wait_ms={})",
                            self.config.max_wait_ms
                        )));
                    }
                    self.emit(&Event::Waiting { ms });
                    self.clock.sleep(d);
                    continue;
                }
                Selection::None(reason) => return Err(Error::NoUsableKeys(reason)),
            };
            let Some(entry) = self.keys.get(idx) else {
                return Err(Error::State(format!(
                    "selected key index {idx} out of range"
                )));
            };

            attempts = attempts.saturating_add(1);
            let body = matches!(request.method, Method::Post).then_some(&request.body);
            let mut verdict = match self.transport.send(request.method, &url, &entry.key, body) {
                Ok(resp) => classify(resp),
                Err(err) => classify_transport(&err),
            };
            // Polling an agent run returns its cumulative cost on every GET;
            // only requests that bill count toward `spent_usd`.
            if !request.bills
                && let Verdict::Success { cost, .. } = &mut verdict
            {
                *cost = 0.0;
            }
            self.emit(&Event::Attempt {
                attempt: attempts,
                label: entry.label.clone(),
                summary: verdict.summary(),
            });
            let now = self.clock.now_ms();
            self.store
                .update(|s| apply(s, &entry.id, &verdict, now, self.config))?;

            match verdict {
                Verdict::Success { body, .. } => return Ok(body),
                Verdict::RequestError {
                    status,
                    tag,
                    message,
                } => {
                    return Err(Error::Request {
                        status,
                        tag,
                        message,
                    });
                }
                Verdict::KeyExhausted { tag, message } => {
                    last_error = format!("402 {tag}: {message}");
                }
                Verdict::KeyInvalid { message } => {
                    last_error = format!("401: {message}");
                }
                Verdict::RateLimited { message, .. } => {
                    last_error = format!("429: {message}");
                }
                Verdict::Transient { message } => {
                    last_error = message;
                    let ms = backoff_ms(attempts);
                    self.emit(&Event::Backoff { ms });
                    self.clock.sleep(Duration::from_millis(ms));
                }
            }
        }
    }
}

fn backoff_ms(attempt: u32) -> u64 {
    let exp = attempt.saturating_sub(1).min(16);
    BASE_BACKOFF_MS
        .saturating_mul(1_u64 << exp)
        .min(MAX_BACKOFF_MS)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use serde_json::json;

    use super::*;
    use crate::transport::{HttpResponse, TransportError};

    struct FakeClock {
        now: Cell<u64>,
        slept: RefCell<Vec<u64>>,
    }

    impl FakeClock {
        fn new(now: u64) -> Self {
            Self {
                now: Cell::new(now),
                slept: RefCell::new(Vec::new()),
            }
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn sleep(&self, d: Duration) {
            let ms = u64::try_from(d.as_millis()).unwrap();
            self.slept.borrow_mut().push(ms);
            self.now.set(self.now.get().saturating_add(ms));
        }
    }

    type Scripted = std::result::Result<HttpResponse, TransportError>;

    struct FakeTransport {
        /// Keyed by api key: responses handed out in order, last one repeats.
        script: RefCell<Vec<(String, Vec<Scripted>)>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeTransport {
        fn new(script: Vec<(&str, Vec<Scripted>)>) -> Self {
            Self {
                script: RefCell::new(script.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Transport for FakeTransport {
        fn send(
            &self,
            _method: Method,
            _url: &str,
            api_key: &str,
            _body: Option<&serde_json::Value>,
        ) -> std::result::Result<HttpResponse, TransportError> {
            self.calls.borrow_mut().push(api_key.to_owned());
            let mut script = self.script.borrow_mut();
            let (_, responses) = script
                .iter_mut()
                .find(|(k, _)| k == api_key)
                .unwrap_or_else(|| panic!("unexpected key {api_key}"));
            if responses.len() > 1 {
                responses.remove(0)
            } else {
                responses[0].clone()
            }
        }
    }

    fn ok() -> Scripted {
        status(200, r#"{"results":[],"costDollars":{"total":0.01}}"#)
    }

    #[allow(clippy::unnecessary_wraps)] // scripted responses share one type with transport errors
    fn status(code: u16, body: &str) -> Scripted {
        Ok(HttpResponse {
            status: code,
            retry_after: None,
            body: body.into(),
        })
    }

    fn config(keys: &[&str]) -> Config {
        Config {
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
            max_attempts: 6,
            max_consecutive_failures: 3,
            quarantine_seconds: 300,
            rate_limit_cooldown_ms: 1_000,
            max_wait_ms: 10_000,
            ..Config::default()
        }
    }

    fn request() -> Request {
        crate::exa::raw(Method::Post, "/search", json!({ "query": "q" }))
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: StateStore,
        config: Config,
        clock: FakeClock,
        events: RefCell<Vec<Event>>,
    }

    impl Harness {
        fn new(keys: &[&str]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = StateStore::new(dir.path().join("state.json"));
            Self {
                _dir: dir,
                store,
                config: config(keys),
                clock: FakeClock::new(1_000_000),
                events: RefCell::new(Vec::new()),
            }
        }

        fn run(&self, transport: &FakeTransport) -> Result<String> {
            self.run_request(transport, &request())
        }

        fn run_request(&self, transport: &FakeTransport, req: &Request) -> Result<String> {
            let observer = |e: &Event| self.events.borrow_mut().push(e.clone());
            Pool::new(&self.config, &self.store, transport, &self.clock)
                .with_observer(&observer)
                .execute(req)
        }

        fn key_state(&self, key: &str) -> KeyState {
            self.store
                .load()
                .unwrap()
                .keys
                .get(&fingerprint(key))
                .cloned()
                .unwrap_or_default()
        }
    }

    #[test]
    fn round_robin_advances_across_invocations() {
        let h = Harness::new(&["k1", "k2", "k3"]);
        let t = FakeTransport::new(vec![
            ("k1", vec![ok()]),
            ("k2", vec![ok()]),
            ("k3", vec![ok()]),
        ]);
        for _ in 0..4 {
            h.run(&t).unwrap();
        }
        assert_eq!(*t.calls.borrow(), vec!["k1", "k2", "k3", "k1"]);
        assert_eq!(h.store.load().unwrap().cursor, 1);
        assert_eq!(h.key_state("k1").ok_requests, 2);
        assert!((h.key_state("k1").spent_usd - 0.02).abs() < 1e-9);
    }

    #[test]
    fn unbilled_requests_do_not_accumulate_spend() {
        let h = Harness::new(&["k1"]);
        let t = FakeTransport::new(vec![(
            "k1",
            vec![status(
                200,
                r#"{"id":"run_1","status":"running","costDollars":{"total":4.5}}"#,
            )],
        )]);
        let poll = crate::exa::agent_get("run_1");
        assert_eq!(poll.method, Method::Get);
        for _ in 0..3 {
            h.run_request(&t, &poll).unwrap();
        }
        assert_eq!(h.key_state("k1").ok_requests, 3);
        assert!(h.key_state("k1").spent_usd.abs() < 1e-9);
    }

    #[test]
    fn exhausted_key_is_marked_and_skipped_forever() {
        let h = Harness::new(&["k1", "k2"]);
        let t = FakeTransport::new(vec![
            (
                "k1",
                vec![status(
                    402,
                    r#"{"error":"none left","tag":"NO_MORE_CREDITS"}"#,
                )],
            ),
            ("k2", vec![ok()]),
        ]);
        h.run(&t).unwrap();
        h.run(&t).unwrap();
        h.run(&t).unwrap();
        assert_eq!(*t.calls.borrow(), vec!["k1", "k2", "k2", "k2"]);
        let ks = h.key_state("k1");
        assert_eq!(ks.status, KeyStatus::Exhausted);
        assert!(ks.last_error.unwrap().contains("NO_MORE_CREDITS"));
    }

    #[test]
    fn invalid_key_is_marked_invalid() {
        let h = Harness::new(&["bad", "good"]);
        let t = FakeTransport::new(vec![
            (
                "bad",
                vec![status(
                    401,
                    r#"{"error":"Invalid API key","tag":"INVALID_API_KEY"}"#,
                )],
            ),
            ("good", vec![ok()]),
        ]);
        h.run(&t).unwrap();
        assert_eq!(h.key_state("bad").status, KeyStatus::Invalid);
        assert_eq!(h.key_state("good").status, KeyStatus::Active);
    }

    #[test]
    fn rate_limit_cools_down_and_rotates_without_strike() {
        let h = Harness::new(&["k1", "k2"]);
        let t = FakeTransport::new(vec![
            ("k1", vec![status(429, r#"{"error":"slow down"}"#), ok()]),
            ("k2", vec![ok()]),
        ]);
        h.run(&t).unwrap();
        assert_eq!(*t.calls.borrow(), vec!["k1", "k2"]);
        let ks = h.key_state("k1");
        assert_eq!(ks.status, KeyStatus::Active);
        assert_eq!(ks.consecutive_failures, 0);
        assert_eq!(ks.cooldown_until_ms, Some(1_000_000 + 1_000));
        assert!(h.clock.slept.borrow().is_empty());
    }

    #[test]
    fn retry_after_header_sets_cooldown() {
        let h = Harness::new(&["k1", "k2"]);
        let t = FakeTransport::new(vec![
            (
                "k1",
                vec![Ok(HttpResponse {
                    status: 429,
                    retry_after: Some(Duration::from_secs(7)),
                    body: "{}".into(),
                })],
            ),
            ("k2", vec![ok()]),
        ]);
        h.run(&t).unwrap();
        assert_eq!(h.key_state("k1").cooldown_until_ms, Some(1_000_000 + 7_000));
    }

    #[test]
    fn all_cooling_waits_then_succeeds() {
        let h = Harness::new(&["k1"]);
        let t = FakeTransport::new(vec![("k1", vec![status(429, "{}"), ok()])]);
        h.run(&t).unwrap();
        assert_eq!(*h.clock.slept.borrow(), vec![1_000]);
        assert!(
            h.events
                .borrow()
                .iter()
                .any(|e| matches!(e, Event::Waiting { ms: 1_000 }))
        );
    }

    #[test]
    fn transient_failures_quarantine_after_three() {
        let h = Harness::new(&["k1"]);
        let t = FakeTransport::new(vec![("k1", vec![status(503, "down")])]);
        let err = h.run(&t).unwrap_err();
        // three transient attempts, then the key is quarantined for 300s > max_wait
        assert_eq!(*t.calls.borrow(), vec!["k1", "k1", "k1"]);
        assert!(matches!(err, Error::NoUsableKeys(_)), "{err}");
        let ks = h.key_state("k1");
        assert_eq!(ks.status, KeyStatus::Active, "quarantine is not exhaustion");
        assert_eq!(ks.consecutive_failures, 0);
        assert!(ks.cooldown_until_ms.unwrap() > 1_000_000 + 299_000);
        assert_eq!(*h.clock.slept.borrow(), vec![250, 500, 1_000]);
    }

    #[test]
    fn transient_rotates_to_next_key_and_success_resets_counter() {
        let h = Harness::new(&["k1", "k2"]);
        let t = FakeTransport::new(vec![
            ("k1", vec![Err(TransportError::Timeout), ok()]),
            ("k2", vec![ok()]),
        ]);
        h.run(&t).unwrap();
        assert_eq!(*t.calls.borrow(), vec!["k1", "k2"]);
        assert_eq!(h.key_state("k1").consecutive_failures, 1);
        h.run(&t).unwrap(); // cursor is back at k1
        assert_eq!(h.key_state("k1").consecutive_failures, 0);
    }

    #[test]
    fn attempt_budget_yields_upstream_error() {
        let h = Harness::new(&["k1", "k2", "k3", "k4", "k5", "k6", "k7"]);
        let t = FakeTransport::new(
            ["k1", "k2", "k3", "k4", "k5", "k6", "k7"]
                .into_iter()
                .map(|k| (k, vec![status(502, "bad gateway")]))
                .collect(),
        );
        let err = h.run(&t).unwrap_err();
        assert!(matches!(err, Error::Upstream { attempts: 6, .. }), "{err}");
    }

    #[test]
    fn request_error_stops_immediately_and_leaves_key_alone() {
        let h = Harness::new(&["k1", "k2"]);
        let t = FakeTransport::new(vec![
            (
                "k1",
                vec![status(
                    400,
                    r#"{"error":"bad body","tag":"INVALID_REQUEST_BODY"}"#,
                )],
            ),
            ("k2", vec![ok()]),
        ]);
        let err = h.run(&t).unwrap_err();
        assert_eq!(
            err,
            Error::Request {
                status: 400,
                tag: Some("INVALID_REQUEST_BODY".into()),
                message: "bad body".into()
            }
        );
        assert_eq!(*t.calls.borrow(), vec!["k1"]);
        assert_eq!(
            h.key_state("k1"),
            KeyState {
                last_used_at_ms: Some(1_000_000),
                ..KeyState::default()
            }
        );
    }

    #[test]
    fn all_exhausted_is_no_usable_keys() {
        let h = Harness::new(&["k1", "k2"]);
        let body = r#"{"error":"x","tag":"TEAM_BUDGET_EXCEEDED"}"#;
        let t = FakeTransport::new(vec![
            ("k1", vec![status(402, body)]),
            ("k2", vec![status(402, body)]),
        ]);
        let err = h.run(&t).unwrap_err();
        assert!(
            matches!(err, Error::NoUsableKeys(ref msg) if msg.contains("2 exhausted")),
            "{err}"
        );
        let err = h.run(&t).unwrap_err();
        assert!(matches!(err, Error::NoUsableKeys(_)));
        assert_eq!(
            t.calls.borrow().len(),
            2,
            "exhausted keys are never retried"
        );
    }

    #[test]
    fn empty_pool() {
        let h = Harness::new(&[]);
        let t = FakeTransport::new(vec![]);
        let err = h.run(&t).unwrap_err();
        assert!(matches!(err, Error::NoUsableKeys(msg) if msg.contains("no api keys")));
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(1), 250);
        assert_eq!(backoff_ms(2), 500);
        assert_eq!(backoff_ms(5), 4_000);
        assert_eq!(backoff_ms(60), 4_000);
    }
}
