//! `exa-pool` binary: the only place that prints.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use exa_pool::cli::{AgentAction, AgentRunArgs, Cli, Command, KeysAction, RawArgs};
use exa_pool::config::{self, ENV_KEYS, Paths};
use exa_pool::error::{Error, Result};
use exa_pool::exa::{self, AgentRunParams, Request};
use exa_pool::generated::AgentRunStatus;
use exa_pool::pool::{Clock, Event, KeyEntry, Pool, SystemClock};
use exa_pool::state::{KeyState, KeyStatus, StateStore};
use exa_pool::transport::UreqTransport;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("exa-pool: {err}");
            ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve(cli.home)?;
    let quiet = cli.quiet;
    let compact = cli.compact;
    let request = match cli.command {
        Command::Keys { action } => return keys(&paths, action),
        Command::Status { json } => return status(&paths, json),
        Command::Agent {
            action: AgentAction::Run(args),
        } => return agent_run(&paths, &args, quiet, compact),
        Command::Agent { action } => agent_request(action),
        Command::Search(args) => exa::search(&args.into())?,
        Command::Contents(args) => exa::contents(&args.into())?,
        Command::Answer(args) => exa::answer(&args.into())?,
        Command::FindSimilar(args) => exa::find_similar(&args.into())?,
        Command::Raw(args) => raw_request(args)?,
    };
    let body = with_pool(&paths, quiet, |pool| pool.execute(&request))?;
    print_body(&body, compact)
}

fn agent_request(action: AgentAction) -> Request {
    match action {
        AgentAction::Run(_) => unreachable!("handled by agent_run"),
        AgentAction::Get { id } => exa::agent_get(&id),
        AgentAction::List { limit, cursor } => exa::agent_list(limit, cursor.as_deref()),
        AgentAction::Events { id, limit, cursor } => {
            exa::agent_events(&id, limit, cursor.as_deref())
        }
        AgentAction::Cancel { id } => exa::agent_cancel(&id),
        AgentAction::Stop { id } => exa::agent_stop(&id),
    }
}

/// Create a run; with `--wait`, poll it until it reaches a terminal status.
fn agent_run(paths: &Paths, args: &AgentRunArgs, quiet: bool, compact: bool) -> Result<()> {
    let create = exa::agent_run(&AgentRunParams::from(args))?;
    let interval = Duration::from_secs(args.poll_interval);
    let body = with_pool(paths, quiet, |pool| {
        let created = pool.execute(&create)?;
        let run = parse_run(&created)?;
        // Always surface the id first: Ctrl-C during --wait must not lose it.
        eprintln!("exa-pool: agent run {} {}", run.id, run.status);
        if !args.wait {
            return Ok(created);
        }
        let poll = exa::agent_get(&run.id);
        // Polls do not bill; one final read after the run settles records
        // its total cost against the key exactly once. That read also
        // happens when the run was already terminal at creation.
        let mut settled = exa::agent_get(&run.id);
        settled.bills = true;
        let mut last = run.status;
        while !is_terminal(&last) {
            std::thread::sleep(interval);
            let body = pool.execute(&poll)?;
            let run = parse_run(&body)?;
            if run.status != last && !quiet {
                eprintln!("exa-pool: agent run {} {}", run.id, run.status);
            }
            last = run.status;
        }
        pool.execute(&settled)
    })?;
    print_body(&body, compact)
}

/// The two fields of an agent run the CLI acts on.
#[derive(serde::Deserialize)]
struct RunHead {
    id: String,
    status: AgentRunStatus,
}

/// Whether a run status can no longer change.
const fn is_terminal(status: &AgentRunStatus) -> bool {
    matches!(
        status,
        AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled
    )
}

fn parse_run(body: &str) -> Result<RunHead> {
    serde_json::from_str(body)
        .map_err(|e| Error::Input(format!("unexpected agent run response: {e}")))
}

fn raw_request(args: RawArgs) -> Result<Request> {
    let text = match (args.body, args.body_file) {
        (Some(inline), _) => inline,
        (None, Some(path)) if path == Path::new("-") => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| Error::Input(format!("stdin: {e}")))?;
            buf
        }
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| Error::Input(format!("{}: {e}", path.display())))?,
        (None, None) => "{}".to_owned(),
    };
    let body = serde_json::from_str(&text).map_err(|e| Error::Input(format!("body: {e}")))?;
    Ok(exa::raw(args.method, &args.path, body))
}

/// Build the pool from config and state, then run `f` with it.
fn with_pool<R>(
    paths: &Paths,
    quiet: bool,
    f: impl FnOnce(&Pool<'_, UreqTransport, SystemClock>) -> Result<R>,
) -> Result<R> {
    let config = config::load(paths)?;
    if config.keys.is_empty() {
        return Err(Error::Config(format!(
            "no api keys; run `exa-pool keys add <KEY>` or set {ENV_KEYS}"
        )));
    }
    let store = StateStore::new(paths.state.clone());
    let transport = UreqTransport::new(Duration::from_secs(config.timeout_seconds));
    let clock = SystemClock;
    let observer = move |event: &Event| {
        if quiet {
            return;
        }
        match event {
            Event::Attempt {
                attempt,
                label,
                summary,
            } => eprintln!("exa-pool: attempt {attempt} key {label}: {summary}"),
            Event::Waiting { ms } => eprintln!("exa-pool: all keys cooling down, waiting {ms}ms"),
            Event::Backoff { ms } => eprintln!("exa-pool: backing off {ms}ms"),
        }
    };
    let pool = Pool::new(&config, &store, &transport, &clock).with_observer(&observer);
    f(&pool)
}

fn print_body(body: &str, compact: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    let rendered = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) if compact => serde_json::to_string(&value).unwrap_or_else(|_| body.to_owned()),
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_owned()),
        Err(_) => body.to_owned(),
    };
    writeln!(out, "{rendered}").map_err(|e| Error::State(format!("stdout: {e}")))
}

// ---------------------------------------------------------------------------
// keys
// ---------------------------------------------------------------------------

fn keys(paths: &Paths, action: KeysAction) -> Result<()> {
    match action {
        KeysAction::List => status(paths, false),
        KeysAction::Add { keys } => keys_add(paths, keys),
        KeysAction::Remove { key } => keys_remove(paths, &key),
        KeysAction::Reset { key } => keys_reset(paths, key.as_deref()),
    }
}

fn keys_add(paths: &Paths, new_keys: Vec<String>) -> Result<()> {
    let mut file = config::load_file(&paths.config)?;
    let before = file.keys.len();
    for key in new_keys {
        let key = key.trim().to_owned();
        if key.is_empty() {
            return Err(Error::Input("empty key".into()));
        }
        if !file.keys.contains(&key) {
            file.keys.push(key);
        }
    }
    config::save(paths, &file)?;
    let added = file.keys.len().saturating_sub(before);
    eprintln!(
        "exa-pool: added {added} key(s), {} total in {}",
        file.keys.len(),
        paths.config.display()
    );
    Ok(())
}

/// Resolve a user-supplied identifier against a key list.
fn find_key<'a>(keys: &'a [String], ident: &str) -> Result<&'a String> {
    let ident = ident.trim();
    let matches: Vec<&String> = keys
        .iter()
        .filter(|k| {
            let entry = KeyEntry::new(k);
            *k == ident || entry.id == ident || entry.label == ident
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::Input(format!("no key matches {ident:?}"))),
        _ => Err(Error::Input(format!(
            "{ident:?} matches {} keys; use the fingerprint",
            matches.len()
        ))),
    }
}

fn keys_remove(paths: &Paths, ident: &str) -> Result<()> {
    let mut file = config::load_file(&paths.config)?;
    let target = find_key(&file.keys, ident)?.clone();
    file.keys.retain(|k| *k != target);
    config::save(paths, &file)?;
    let entry = KeyEntry::new(&target);
    let store = StateStore::new(paths.state.clone());
    store.update(|s| {
        s.keys.remove(&entry.id);
    })?;
    eprintln!("exa-pool: removed {} ({})", entry.label, entry.id);
    Ok(())
}

fn keys_reset(paths: &Paths, ident: Option<&str>) -> Result<()> {
    let config = config::load(paths)?;
    let targets: Vec<KeyEntry> = match ident {
        Some(ident) => vec![KeyEntry::new(find_key(&config.keys, ident)?)],
        None => config.keys.iter().map(|k| KeyEntry::new(k)).collect(),
    };
    let now = SystemClock.now_ms();
    let store = StateStore::new(paths.state.clone());
    let count = store.update(|s| {
        let mut n = 0_usize;
        for entry in &targets {
            if let Some(ks) = s.keys.get_mut(&entry.id) {
                ks.status = KeyStatus::Active;
                ks.status_changed_at_ms = Some(now);
                ks.cooldown_until_ms = None;
                ks.consecutive_failures = 0;
                ks.last_error = None;
                n = n.saturating_add(1);
            }
        }
        n
    })?;
    eprintln!("exa-pool: reset {count} key(s)");
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn status(paths: &Paths, json: bool) -> Result<()> {
    let config = config::load(paths)?;
    let file_keys = config::load_file(&paths.config)?.keys;
    let state = StateStore::new(paths.state.clone()).load()?;
    let now = SystemClock.now_ms();
    let entries: Vec<KeyEntry> = config.keys.iter().map(|k| KeyEntry::new(k)).collect();
    let next = if entries.is_empty() {
        None
    } else {
        let idx = usize::try_from(state.cursor)
            .unwrap_or(0)
            .checked_rem(entries.len())
            .unwrap_or(0);
        entries.get(idx).map(|e| e.label.clone())
    };

    let rows: Vec<Row> = entries
        .iter()
        .map(|e| {
            let ks = state.keys.get(&e.id).cloned().unwrap_or_default();
            Row::new(e, &ks, now, file_keys.contains(&e.key))
        })
        .collect();

    let mut out = io::stdout().lock();
    if json {
        let value = serde_json::json!({
            "config": paths.config,
            "state": paths.state,
            "next": next,
            "keys": rows,
        });
        let text = serde_json::to_string_pretty(&value).map_err(|e| Error::State(e.to_string()))?;
        writeln!(out, "{text}").map_err(|e| Error::State(e.to_string()))?;
        return Ok(());
    }

    writeln!(out, "config: {}", paths.config.display()).map_err(|e| io_err(&e))?;
    writeln!(out, "state:  {}", paths.state.display()).map_err(|e| io_err(&e))?;
    if rows.is_empty() {
        writeln!(out, "no keys configured").map_err(|e| io_err(&e))?;
        return Ok(());
    }
    writeln!(out, "next:   {}", next.unwrap_or_default()).map_err(|e| io_err(&e))?;
    writeln!(
        out,
        "{:<11} {:<16} {:<9} {:<6} {:<10} {:>5} {:>9}  LAST ERROR",
        "KEY", "FINGERPRINT", "STATUS", "SRC", "COOLDOWN", "OK", "SPENT"
    )
    .map_err(|e| io_err(&e))?;
    for r in &rows {
        writeln!(
            out,
            "{:<11} {:<16} {:<9} {:<6} {:<10} {:>5} {:>9}  {}",
            r.label,
            r.fingerprint,
            r.status,
            r.source,
            r.cooldown.as_deref().unwrap_or("-"),
            r.ok_requests,
            format!("${:.4}", r.spent_usd),
            r.last_error.as_deref().unwrap_or("-"),
        )
        .map_err(|e| io_err(&e))?;
    }
    Ok(())
}

fn io_err(e: &io::Error) -> Error {
    Error::State(format!("stdout: {e}"))
}

#[derive(serde::Serialize)]
struct Row {
    label: String,
    fingerprint: String,
    status: &'static str,
    source: &'static str,
    cooldown: Option<String>,
    cooldown_until_ms: Option<u64>,
    consecutive_failures: u32,
    ok_requests: u64,
    spent_usd: f64,
    last_error: Option<String>,
}

impl Row {
    fn new(entry: &KeyEntry, ks: &KeyState, now: u64, from_file: bool) -> Self {
        let status = match ks.status {
            KeyStatus::Active if !ks.is_eligible(now) => "cooling",
            KeyStatus::Active => "active",
            KeyStatus::Exhausted => "exhausted",
            KeyStatus::Invalid => "invalid",
        };
        let cooldown = ks
            .cooldown_until_ms
            .filter(|t| *t > now)
            .map(|t| human_ms(t.saturating_sub(now)));
        Self {
            label: entry.label.clone(),
            fingerprint: entry.id.clone(),
            status,
            source: if from_file { "file" } else { "env" },
            cooldown,
            cooldown_until_ms: ks.cooldown_until_ms,
            consecutive_failures: ks.consecutive_failures,
            ok_requests: ks.ok_requests,
            spent_usd: ks.spent_usd,
            last_error: ks.last_error.as_deref().map(shorten),
        }
    }
}

fn human_ms(ms: u64) -> String {
    let secs = ms.div_ceil(1_000);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn shorten(s: &str) -> String {
    const LIMIT: usize = 60;
    let mut out: String = s.chars().take(LIMIT).collect();
    if s.chars().count() > LIMIT {
        out.push('…');
    }
    out
}
