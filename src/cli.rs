//! Command-line surface (clap derive).
//!
//! Enum-valued flags (`--type`, `--category`, `--livecrawl`, `--model`,
//! `--effort`, `--data-source`) are parsed into the generated spec enums, so an
//! unknown value is rejected locally with the list the spec allows.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use clap::{Args, Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::ENV_HOME;
use crate::exa::{
    AgentRunParams, AnswerParams, ContentsChoice, ContentsParams, FindSimilarParams, SearchParams,
};
use crate::generated::{
    AgentDataSourceProvider, AgentEffort, AnswerRequestModel, ContentsOptionsLivecrawl,
    FindSimilarRequestCategory, SearchRequestCategory, SearchRequestTypeInstant,
};
use crate::transport::Method;

/// Exa API CLI backed by a round-robin pool of API keys.
#[derive(Debug, Parser)]
#[command(name = "exa-search", version, about, long_about = None)]
pub struct Cli {
    /// Directory holding `config.toml` and `state.json`.
    #[arg(long, global = true, value_name = "DIR", env = ENV_HOME)]
    pub home: Option<PathBuf>,

    /// Suppress per-attempt diagnostics on stderr.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Print JSON on a single line instead of pretty-printed.
    #[arg(long, global = true)]
    pub compact: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// POST /search
    Search(SearchArgs),
    /// POST /contents
    Contents(ContentsArgs),
    /// POST /answer (non-streaming)
    Answer(AnswerArgs),
    /// POST /findSimilar
    FindSimilar(FindSimilarArgs),
    /// Exa Agent runs (/agent/runs): async research and list building
    Agent {
        /// Action to perform.
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Send any path with a raw JSON body (for endpoints not wrapped here)
    Raw(RawArgs),
    /// Manage the key pool
    Keys {
        /// Action to perform.
        #[command(subcommand)]
        action: KeysAction,
    },
    /// Show pool health
    Status {
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Parse a string into a spec enum through its serde representation.
///
/// # Errors
/// Returns serde's message, which lists every accepted value.
pub fn parse_enum<T: DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(s.to_owned())).map_err(|e| e.to_string())
}

/// Accept RFC 3339 (`2024-01-02T03:04:05Z`) or a bare date (`2024-01-02`, UTC midnight).
///
/// # Errors
/// When neither form parses.
pub fn parse_date(s: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
        .ok_or_else(|| format!("{s:?} is not RFC 3339 or YYYY-MM-DD"))
}

/// Accept inline JSON or `@path` to read JSON from a file.
///
/// # Errors
/// When the file cannot be read or the text is not JSON.
pub fn parse_json(s: &str) -> Result<Value, String> {
    let text = match s.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        None => s.to_owned(),
    };
    serde_json::from_str(&text).map_err(|e| format!("not JSON: {e}"))
}

/// `GET` or `POST`, case-insensitive.
///
/// # Errors
/// For any other verb.
pub fn parse_method(s: &str) -> Result<Method, String> {
    match s.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        other => Err(format!("{other:?} is not GET or POST")),
    }
}

/// `KEY=VALUE`.
///
/// # Errors
/// When there is no `=` or the key is empty.
pub fn parse_key_value(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_owned(), v.to_owned())),
        _ => Err(format!("{s:?} is not KEY=VALUE")),
    }
}

fn positive() -> clap::builder::RangedI64ValueParser<i64> {
    clap::value_parser!(i64).range(1..)
}

/// Parse an agent spend cap; the spec bounds `maxCostDollars` to `1..=100`.
///
/// # Errors
/// When the value is not a number in that range.
pub fn parse_max_cost(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("{s:?}: {e}"))?;
    if (1.0..=100.0).contains(&v) {
        Ok(v)
    } else {
        Err(format!("{s} is outside 1..=100"))
    }
}

/// Contents flags shared by search, contents, and find-similar.
#[derive(Debug, Args, Default, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent on/off content option, not a state machine"
)]
pub struct ContentsFlags {
    /// Include full page text.
    #[arg(long, help_heading = "Contents")]
    pub text: bool,

    /// Cap on text characters per result (implies --text).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub max_characters: Option<i64>,

    /// Include highlight snippets.
    #[arg(long, help_heading = "Contents")]
    pub highlights: bool,

    /// Question the highlights should answer (implies --highlights).
    #[arg(long, value_name = "QUERY", help_heading = "Contents")]
    pub highlights_query: Option<String>,

    /// Sentences per highlight (implies --highlights).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_num_sentences: Option<i64>,

    /// Highlights per result (implies --highlights).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_per_url: Option<i64>,

    /// Cap on highlight characters per result (implies --highlights).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_max_characters: Option<i64>,

    /// Include an LLM summary.
    #[arg(long, help_heading = "Contents")]
    pub summary: bool,

    /// Focus question for the summary (implies --summary).
    #[arg(long, value_name = "QUERY", help_heading = "Contents")]
    pub summary_query: Option<String>,

    /// JSON schema the summary must follow, inline or @file (implies --summary).
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json, help_heading = "Contents")]
    pub summary_schema: Option<Value>,

    /// Include one LLM-ready context string built from all results.
    #[arg(long, help_heading = "Contents")]
    pub context: bool,

    /// Cap on context characters (implies --context).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub context_max_characters: Option<i64>,

    /// Livecrawl mode (spec enum: never, always, fallback, preferred).
    #[arg(long, value_name = "MODE", value_parser = parse_enum::<ContentsOptionsLivecrawl>, help_heading = "Contents")]
    pub livecrawl: Option<ContentsOptionsLivecrawl>,

    /// Livecrawl timeout in milliseconds.
    #[arg(long, value_name = "MS", value_parser = positive(), help_heading = "Contents")]
    pub livecrawl_timeout: Option<i64>,

    /// Accept cached pages no older than this many hours.
    #[arg(long, value_name = "HOURS", value_parser = positive(), help_heading = "Contents")]
    pub max_age_hours: Option<i64>,

    /// Subpages to crawl per result.
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub subpages: Option<i64>,

    /// Keywords that choose which subpages to crawl (repeat or comma-separate).
    #[arg(
        long,
        value_name = "WORD",
        value_delimiter = ',',
        help_heading = "Contents"
    )]
    pub subpage_target: Vec<String>,

    /// Outbound links to return per result.
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub extras_links: Option<i64>,

    /// Image links to return per result.
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub extras_image_links: Option<i64>,
}

impl From<ContentsFlags> for ContentsChoice {
    fn from(f: ContentsFlags) -> Self {
        Self {
            text: f.text,
            max_characters: f.max_characters,
            highlights: f.highlights,
            highlights_query: f.highlights_query,
            highlights_num_sentences: f.highlights_num_sentences,
            highlights_per_url: f.highlights_per_url,
            highlights_max_characters: f.highlights_max_characters,
            summary: f.summary,
            summary_query: f.summary_query,
            summary_schema: f.summary_schema,
            context: f.context,
            context_max_characters: f.context_max_characters,
            livecrawl: f.livecrawl,
            livecrawl_timeout: f.livecrawl_timeout,
            max_age_hours: f.max_age_hours,
            subpages: f.subpages,
            subpage_target: f.subpage_target,
            extras_links: f.extras_links,
            extras_image_links: f.extras_image_links,
        }
    }
}

/// Arguments for `search`.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Natural-language query.
    pub query: String,

    /// Number of results, 1..=100.
    #[arg(short = 'n', long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
    pub num_results: Option<i64>,

    /// Search type (spec enum: instant, fast, auto, deep-lite, deep, deep-reasoning).
    #[arg(short = 't', long = "type", value_name = "TYPE", value_parser = parse_enum::<SearchRequestTypeInstant>)]
    pub search_type: Option<SearchRequestTypeInstant>,

    /// Category (spec enum: company, publication, news, "personal site", "financial report", people).
    #[arg(short = 'c', long, value_name = "CATEGORY", value_parser = parse_enum::<SearchRequestCategory>)]
    pub category: Option<SearchRequestCategory>,

    /// Only these domains (repeat or comma-separate).
    #[arg(
        long,
        value_name = "DOMAIN",
        value_delimiter = ',',
        help_heading = "Filters"
    )]
    pub include_domains: Vec<String>,

    /// Never these domains (repeat or comma-separate).
    #[arg(
        long,
        value_name = "DOMAIN",
        value_delimiter = ',',
        help_heading = "Filters"
    )]
    pub exclude_domains: Vec<String>,

    /// Published on or after (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub start_published_date: Option<DateTime<Utc>>,

    /// Published on or before (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub end_published_date: Option<DateTime<Utc>>,

    /// Crawled on or after (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub start_crawl_date: Option<DateTime<Utc>>,

    /// Crawled on or before (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub end_crawl_date: Option<DateTime<Utc>>,

    /// Phrase that must appear in page text (repeatable). Not in the published
    /// spec; Exa's SDK documents one phrase of up to five words.
    #[arg(long, value_name = "PHRASE", help_heading = "Filters")]
    pub include_text: Vec<String>,

    /// Phrase that must not appear in page text (repeatable). Same caveat as --include-text.
    #[arg(long, value_name = "PHRASE", help_heading = "Filters")]
    pub exclude_text: Vec<String>,

    /// Drop results that fail content moderation.
    #[arg(long)]
    pub moderation: bool,

    /// Extra query variation (repeatable).
    #[arg(long, value_name = "QUERY")]
    pub additional_query: Vec<String>,

    /// Guidance for the request (source preferences, constraints).
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Two-letter ISO country code of the user.
    #[arg(long, value_name = "CC")]
    pub user_location: Option<String>,

    /// JSON schema for structured output, inline or @file (deep search types).
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json, conflicts_with = "structured")]
    pub output_schema: Option<Value>,

    /// Shorthand for --output-schema '{"type":"object"}'.
    #[arg(long)]
    pub structured: bool,

    /// Contents to attach.
    #[command(flatten)]
    pub contents: ContentsFlags,
}

impl From<SearchArgs> for SearchParams {
    fn from(a: SearchArgs) -> Self {
        let output_schema = a.output_schema.or_else(|| {
            a.structured
                .then(|| serde_json::json!({ "type": "object" }))
        });
        Self {
            query: a.query,
            search_type: a.search_type,
            category: a.category,
            num_results: a.num_results,
            include_domains: a.include_domains,
            exclude_domains: a.exclude_domains,
            start_published_date: a.start_published_date,
            end_published_date: a.end_published_date,
            start_crawl_date: a.start_crawl_date,
            end_crawl_date: a.end_crawl_date,
            include_text: a.include_text,
            exclude_text: a.exclude_text,
            additional_queries: a.additional_query,
            system_prompt: a.system_prompt,
            user_location: a.user_location,
            moderation: a.moderation.then_some(true),
            output_schema,
            contents: a.contents.into(),
        }
    }
}

/// Arguments for `contents`.
#[derive(Debug, Args)]
pub struct ContentsArgs {
    /// URLs or Exa result ids (1..=100).
    #[arg(required = true, value_name = "URL", num_args = 1..=100)]
    pub urls: Vec<String>,

    /// Contents to fetch (defaults to --text).
    #[command(flatten)]
    pub contents: ContentsFlags,
}

impl From<ContentsArgs> for ContentsParams {
    fn from(a: ContentsArgs) -> Self {
        Self {
            urls: a.urls,
            contents: a.contents.into(),
        }
    }
}

/// Arguments for `answer`.
#[derive(Debug, Args)]
pub struct AnswerArgs {
    /// Question to answer.
    pub query: String,

    /// Include full text of citations.
    #[arg(long)]
    pub text: bool,

    /// Model (spec enum: exa, exa-pro, exa-research, exa-fast).
    #[arg(long, value_name = "MODEL", value_parser = parse_enum::<AnswerRequestModel>)]
    pub model: Option<AnswerRequestModel>,

    /// Guidance for the answer.
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,
}

impl From<AnswerArgs> for AnswerParams {
    fn from(a: AnswerArgs) -> Self {
        Self {
            query: a.query,
            text: a.text,
            model: a.model,
            system_prompt: a.system_prompt,
        }
    }
}

/// Arguments for `find-similar`.
#[derive(Debug, Args)]
pub struct FindSimilarArgs {
    /// Seed URL.
    pub url: String,

    /// Number of results, 1..=100.
    #[arg(short = 'n', long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
    pub num_results: Option<i64>,

    /// Category (spec enum).
    #[arg(short = 'c', long, value_name = "CATEGORY", value_parser = parse_enum::<FindSimilarRequestCategory>)]
    pub category: Option<FindSimilarRequestCategory>,

    /// Only these domains.
    #[arg(
        long,
        value_name = "DOMAIN",
        value_delimiter = ',',
        help_heading = "Filters"
    )]
    pub include_domains: Vec<String>,

    /// Never these domains.
    #[arg(
        long,
        value_name = "DOMAIN",
        value_delimiter = ',',
        help_heading = "Filters"
    )]
    pub exclude_domains: Vec<String>,

    /// Omit results from the seed URL's domain.
    #[arg(long, help_heading = "Filters")]
    pub exclude_source_domain: bool,

    /// Published on or after (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub start_published_date: Option<DateTime<Utc>>,

    /// Published on or before (RFC 3339 or YYYY-MM-DD).
    #[arg(long, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub end_published_date: Option<DateTime<Utc>>,

    /// Contents to attach.
    #[command(flatten)]
    pub contents: ContentsFlags,
}

impl From<FindSimilarArgs> for FindSimilarParams {
    fn from(a: FindSimilarArgs) -> Self {
        Self {
            url: a.url,
            num_results: a.num_results,
            category: a.category,
            include_domains: a.include_domains,
            exclude_domains: a.exclude_domains,
            exclude_source_domain: a.exclude_source_domain,
            start_published_date: a.start_published_date,
            end_published_date: a.end_published_date,
            contents: a.contents.into(),
        }
    }
}

/// `agent` subcommands.
#[derive(Debug, Subcommand)]
pub enum AgentAction {
    /// Start a run (POST /agent/runs); prints the run object, or the finished run with --wait
    Run(AgentRunArgs),
    /// Fetch a run (GET /agent/runs/{id})
    Get {
        /// Run id.
        id: String,
    },
    /// List runs (GET /agent/runs)
    List {
        /// Results per page, 1..=100.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
        limit: Option<i64>,
        /// `nextCursor` from the previous page.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Fetch a run's event log (GET /agent/runs/{id}/events)
    Events {
        /// Run id.
        id: String,
        /// Results per page, 1..=100.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
        limit: Option<i64>,
        /// `nextCursor` from the previous page.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Cancel a run (POST /agent/runs/{id}/cancel)
    Cancel {
        /// Run id.
        id: String,
    },
    /// Stop a run and keep partial output (POST /agent/runs/{id}/stop)
    Stop {
        /// Run id.
        id: String,
    },
}

/// Arguments for `agent run`.
#[derive(Debug, Args)]
pub struct AgentRunArgs {
    /// What the agent should research or build.
    pub query: String,

    /// Guidance for the agent.
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Effort (spec enum: minimal, low, medium, high, xhigh, auto, max).
    /// Server default is auto, capped at $5; max is capped at $20.
    #[arg(long, value_name = "EFFORT", value_parser = parse_enum::<AgentEffort>)]
    pub effort: Option<AgentEffort>,

    /// Spend cap in USD (1..=100) for the auto and max efforts.
    #[arg(long, value_name = "USD", value_parser = parse_max_cost)]
    pub max_cost: Option<f64>,

    /// JSON schema for structured output, inline or @file.
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json)]
    pub output_schema: Option<Value>,

    /// Continue from an earlier run.
    #[arg(long, value_name = "RUN_ID")]
    pub previous_run_id: Option<String>,

    /// Exa Connect provider to enable (spec enum, repeatable).
    #[arg(long, value_name = "PROVIDER", value_parser = parse_enum::<AgentDataSourceProvider>)]
    pub data_source: Vec<AgentDataSourceProvider>,

    /// Metadata stored with the run (KEY=VALUE, repeatable).
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub metadata: Vec<(String, String)>,

    /// Poll until the run finishes and print the final run object.
    #[arg(long)]
    pub wait: bool,

    /// Seconds between polls with --wait.
    #[arg(long, value_name = "SECS", default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..))]
    pub poll_interval: u64,
}

impl From<&AgentRunArgs> for AgentRunParams {
    fn from(a: &AgentRunArgs) -> Self {
        Self {
            query: a.query.clone(),
            system_prompt: a.system_prompt.clone(),
            effort: a.effort.clone(),
            max_cost_dollars: a.max_cost,
            output_schema: a.output_schema.clone(),
            previous_run_id: a.previous_run_id.clone(),
            data_sources: a.data_source.clone(),
            metadata: a.metadata.clone(),
        }
    }
}

/// Arguments for `raw`.
#[derive(Debug, Args)]
pub struct RawArgs {
    /// Endpoint path, e.g. `/agent/runs` or `search`.
    pub path: String,

    /// HTTP verb (GET or POST).
    #[arg(short = 'X', long, value_name = "VERB", default_value = "POST", value_parser = parse_method)]
    pub method: Method,

    /// Inline JSON body.
    #[arg(long, value_name = "JSON", conflicts_with = "body_file")]
    pub body: Option<String>,

    /// Read JSON body from a file (`-` for stdin).
    #[arg(long, value_name = "FILE")]
    pub body_file: Option<PathBuf>,
}

/// `keys` subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysAction {
    /// List configured keys (masked) with their health
    List,
    /// Append keys to config.toml
    Add {
        /// One or more API keys.
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
    },
    /// Remove a key from config.toml (full key, fingerprint, or masked label)
    Remove {
        /// Identifier.
        #[arg(value_name = "KEY|FINGERPRINT|LABEL")]
        key: String,
    },
    /// Clear exhausted/invalid/cooldown status (all keys, or one)
    Reset {
        /// Identifier; omit to reset every key.
        #[arg(value_name = "KEY|FINGERPRINT|LABEL")]
        key: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_search_with_spec_enums() {
        let cli = Cli::parse_from([
            "exa-search",
            "search",
            "rust http client",
            "-n",
            "5",
            "--type",
            "deep-lite",
            "--category",
            "personal site",
            "--include-domains",
            "a.com,b.com",
            "--max-characters",
            "200",
            "--start-published-date",
            "2024-03-01",
            "--livecrawl",
            "preferred",
            "--include-text",
            "async runtime",
            "--structured",
            "--subpage-target",
            "docs,pricing",
            "--moderation",
        ]);
        let Command::Search(args) = cli.command else {
            panic!("expected search");
        };
        let params: SearchParams = args.into();
        assert_eq!(params.num_results, Some(5));
        assert_eq!(params.search_type, Some(SearchRequestTypeInstant::DeepLite));
        assert_eq!(params.category, Some(SearchRequestCategory::PersonalSite));
        assert_eq!(params.include_domains, vec!["a.com", "b.com"]);
        assert_eq!(params.contents.max_characters, Some(200));
        assert_eq!(
            params.contents.livecrawl,
            Some(ContentsOptionsLivecrawl::Preferred)
        );
        assert_eq!(params.include_text, vec!["async runtime"]);
        assert_eq!(
            params.output_schema,
            Some(serde_json::json!({ "type": "object" }))
        );
        assert_eq!(params.contents.subpage_target, vec!["docs", "pricing"]);
        assert_eq!(params.moderation, Some(true));
        assert_eq!(
            params.start_published_date.unwrap().to_rfc3339(),
            "2024-03-01T00:00:00+00:00"
        );
    }

    #[test]
    fn unknown_enum_value_is_rejected_with_allowed_list() {
        let err =
            Cli::try_parse_from(["exa-search", "search", "q", "--type", "neural"]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown variant `neural`"), "{msg}");
        assert!(msg.contains("`deep-reasoning`"), "{msg}");
    }

    #[test]
    fn structured_and_output_schema_conflict() {
        let err = Cli::try_parse_from([
            "exa-search",
            "search",
            "q",
            "--structured",
            "--output-schema",
            "{}",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn num_results_range_is_enforced() {
        assert!(Cli::try_parse_from(["exa-search", "search", "q", "-n", "0"]).is_err());
        assert!(Cli::try_parse_from(["exa-search", "search", "q", "-n", "101"]).is_err());
        assert!(Cli::try_parse_from(["exa-search", "search", "q", "-n", "100"]).is_ok());
    }

    #[test]
    fn bad_date_is_rejected() {
        let err = Cli::try_parse_from([
            "exa-search",
            "search",
            "q",
            "--start-published-date",
            "yesterday",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("RFC 3339"));
    }

    #[test]
    fn agent_run_parses_spec_enums_and_metadata() {
        let cli = Cli::parse_from([
            "exa-search",
            "agent",
            "run",
            "list rust http clients",
            "--effort",
            "minimal",
            "--max-cost",
            "2",
            "--data-source",
            "similarweb",
            "--metadata",
            "ticket=42",
            "--wait",
            "--poll-interval",
            "7",
        ]);
        let Command::Agent {
            action: AgentAction::Run(args),
        } = cli.command
        else {
            panic!("expected agent run");
        };
        assert!(args.wait);
        assert_eq!(args.poll_interval, 7);
        let params = AgentRunParams::from(&args);
        assert_eq!(params.effort, Some(AgentEffort::Minimal));
        assert_eq!(params.max_cost_dollars, Some(2.0));
        assert_eq!(
            params.data_sources,
            vec![AgentDataSourceProvider::Similarweb]
        );
        assert_eq!(
            params.metadata,
            vec![("ticket".to_owned(), "42".to_owned())]
        );
    }

    #[test]
    fn agent_max_cost_outside_spec_range_is_rejected() {
        for bad in ["0.5", "101", "nan"] {
            let err = Cli::try_parse_from(["exa-search", "agent", "run", "q", "--max-cost", bad])
                .unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{bad}");
        }
        assert!((parse_max_cost("100").unwrap() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn agent_effort_outside_spec_is_rejected() {
        let err = Cli::try_parse_from(["exa-search", "agent", "run", "q", "--effort", "turbo"])
            .unwrap_err();
        assert!(err.to_string().contains("`xhigh`"), "{err}");
    }

    #[test]
    fn raw_method_and_body_flags() {
        let err = Cli::try_parse_from([
            "exa-search",
            "raw",
            "/x",
            "--body",
            "{}",
            "--body-file",
            "f.json",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
        let cli = Cli::parse_from(["exa-search", "raw", "/agent/runs", "-X", "get"]);
        let Command::Raw(args) = cli.command else {
            panic!("expected raw");
        };
        assert_eq!(args.method, Method::Get);
        assert!(Cli::try_parse_from(["exa-search", "raw", "/x", "-X", "PUT"]).is_err());
    }

    #[test]
    fn json_flag_reads_inline_or_file() {
        assert_eq!(
            parse_json(r#"{"a":1}"#).unwrap(),
            serde_json::json!({"a": 1})
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.json");
        std::fs::write(&path, r#"{"type":"object"}"#).unwrap();
        let arg = format!("@{}", path.display());
        assert_eq!(
            parse_json(&arg).unwrap(),
            serde_json::json!({"type": "object"})
        );
        assert!(parse_json("@/nonexistent/x.json").is_err());
        assert!(parse_json("nope").is_err());
    }
}
