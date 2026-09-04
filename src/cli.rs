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

/// Text appended to the top-level help: how to choose a command and what
/// every command has in common. Kept terse on purpose; it is read by agents.
const TOP_HELP: &str = "\
Start with `search --highlights`; `contents` for URLs you already have; `answer` when only the answer \
matters; `agent run` for open-ended lists or multi-hop research.
Output: JSON on stdout, progress on stderr. Every response carries costDollars.total.
Exit: 0 ok, 1 rejected locally before sending (config or input), 2 usage, 3 Exa rejected the request (fix arguments, do not retry), \
4 no usable key, 5 Exa unavailable after retries.
Keys: `keys add KEY...` or EXA_API_KEYS. Requests rotate across keys; 429 and 5xx are retried for you.";

/// Command-line surface. `about` is spelled out because clap would otherwise
/// print the crate description, which is written for people browsing crates.
#[derive(Debug, Parser)]
#[command(
    name = "exa-search",
    version,
    about = "Exa web search for agents: find pages, read URLs, get cited answers, run async research.",
    long_about = None,
    after_help = TOP_HELP
)]
pub struct Cli {
    /// Directory holding `config.toml` and `state.json`.
    #[arg(long, global = true, value_name = "DIR", env = ENV_HOME)]
    pub home: Option<PathBuf>,

    /// Silence per-attempt progress on stderr.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// One-line JSON instead of pretty-printed.
    #[arg(long, global = true)]
    pub compact: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Find web pages for a natural-language query, optionally with page contents or a synthesized answer.
    ///
    /// Prints {results: [{title, url, id, publishedDate, author, text?, highlights?, summary?}],
    /// output?: {content, grounding}, costDollars}. Result ids equal urls and can be passed to `contents`.
    /// Defaults: --type auto (about 1s), 10 results, no page contents.
    ///
    /// Contents: --highlights returns the relevant excerpts at roughly a tenth of the tokens of --text;
    /// --text --max-characters N returns the full page; --summary returns an abstract. They combine.
    ///
    /// Types: instant and fast trade depth for latency. deep-lite (about 4s), deep (4-15s) and
    /// deep-reasoning (12-40s) run multi-step research and fill output.content with grounding citations.
    ///
    /// Synthesis: --output-schema or --structured works on every type and adds about 2s. Schema limits:
    /// depth 2, 10 properties, no citation fields (grounding is returned separately).
    ///
    /// Categories company and people ignore the date filters and --exclude-domains.
    /// Price: about $7 per 1k requests (deep types $12-15), plus $1 per 1k results and per 1k pages of contents.
    Search(SearchArgs),
    /// Read one or more URLs, or result ids from `search`. Defaults to --text.
    ///
    /// Prints {results: [{url, title, text?, highlights?, summary?, subpages?, extras?}],
    /// statuses: [{id, status, error?}], costDollars}. The request succeeds even when some URLs fail:
    /// check statuses. For JSON per page use --summary-schema; there is no --output-schema here.
    /// Price: $1 per 1k pages per content type.
    Contents(ContentsArgs),
    /// Search, then write one answer with citations.
    ///
    /// Prints {answer, citations: [{url, title, publishedDate, author, text?}], costDollars}.
    /// Use when the answer is the deliverable; use `search` when you need the sources themselves.
    /// Price: $5 per 1k requests.
    Answer(AnswerArgs),
    /// Pages similar to a seed URL. Deprecated by Exa: prefer `search` with a query describing the seed page.
    ///
    /// Prints the same shape as `search`.
    FindSimilar(FindSimilarArgs),
    /// Exa Agent: asynchronous multi-step research, list building and enrichment, priced in dollars.
    Agent {
        /// Action to perform.
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Call any Exa endpoint the other commands do not cover.
    ///
    /// The body is sent unchanged and the response printed as-is. Exa accepts unknown fields
    /// silently, so a typo in a field name is billed and ignored rather than rejected.
    Raw(RawArgs),
    /// Manage the API key pool in config.toml.
    Keys {
        /// Action to perform.
        #[command(subcommand)]
        action: KeysAction,
    },
    /// Per-key health: status, consecutive failures, cooldown, and spend summed from costDollars.
    Status {
        /// Machine-readable JSON instead of a table.
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
    /// Full page text as markdown.
    #[arg(long, help_heading = "Contents")]
    pub text: bool,

    /// Truncate page text at N characters (implies --text).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub max_characters: Option<i64>,

    /// The most relevant excerpts of each page; the token-cheap choice for lookups.
    #[arg(long, help_heading = "Contents")]
    pub highlights: bool,

    /// Question the excerpts should answer; defaults to the query (implies --highlights).
    #[arg(long, value_name = "QUERY", help_heading = "Contents")]
    pub highlights_query: Option<String>,

    /// Deprecated by Exa; use --highlights alone.
    #[arg(long, hide = true, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_num_sentences: Option<i64>,

    /// Deprecated by Exa; use --highlights alone.
    #[arg(long, hide = true, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_per_url: Option<i64>,

    /// Cap excerpt characters per page (implies --highlights).
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub highlights_max_characters: Option<i64>,

    /// LLM abstract of each page.
    #[arg(long, help_heading = "Contents")]
    pub summary: bool,

    /// What the abstract should focus on (implies --summary).
    #[arg(long, value_name = "QUERY", help_heading = "Contents")]
    pub summary_query: Option<String>,

    /// JSON schema the abstract must follow: per-page structured extraction (implies --summary).
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json, help_heading = "Contents")]
    pub summary_schema: Option<Value>,

    /// Deprecated by Exa; use --highlights or --text.
    #[arg(long, hide = true, help_heading = "Contents")]
    pub context: bool,

    /// Deprecated by Exa; use --highlights or --text.
    #[arg(long, hide = true, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub context_max_characters: Option<i64>,

    /// Deprecated by Exa; use --max-age-hours.
    #[arg(long, hide = true, value_name = "MODE", value_parser = parse_enum::<ContentsOptionsLivecrawl>, help_heading = "Contents")]
    pub livecrawl: Option<ContentsOptionsLivecrawl>,

    /// Milliseconds to wait for a live fetch [Exa default: 10000].
    #[arg(long, value_name = "MS", value_parser = positive(), help_heading = "Contents")]
    pub livecrawl_timeout: Option<i64>,

    /// Freshness: reuse cached pages younger than N hours, otherwise fetch live.
    /// 0 always fetches live (slower), -1 never does (fastest), omitted fetches live only when
    /// nothing is cached. Max 720.
    #[arg(
        long,
        value_name = "HOURS",
        allow_negative_numbers = true,
        value_parser = clap::value_parser!(i64).range(-1..=720),
        help_heading = "Contents"
    )]
    pub max_age_hours: Option<i64>,

    /// Also crawl up to N linked pages per result, returned under subpages.
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub subpages: Option<i64>,

    /// Keywords that pick which linked pages to crawl, e.g. docs,pricing (repeat or comma-separate).
    #[arg(
        long,
        value_name = "WORD",
        value_delimiter = ',',
        help_heading = "Contents"
    )]
    pub subpage_target: Vec<String>,

    /// Return up to N outbound links per page.
    #[arg(long, value_name = "N", value_parser = positive(), help_heading = "Contents")]
    pub extras_links: Option<i64>,

    /// Return up to N image URLs per page.
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
    /// Prose description of the pages wanted, e.g. "blog post explaining how Rust async runtimes schedule tasks"; not keywords.
    pub query: String,

    /// Results to return, 1..=100 [default: 10].
    #[arg(short = 'n', long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
    pub num_results: Option<i64>,

    /// instant | fast | auto | deep-lite | deep | deep-reasoning [default: auto].
    #[arg(short = 't', long = "type", value_name = "TYPE", value_parser = parse_enum::<SearchRequestTypeInstant>)]
    pub search_type: Option<SearchRequestTypeInstant>,

    /// Restrict to one kind of page: company | people | publication | news |
    /// "personal site" | "financial report".
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

    /// Deprecated by Exa; ignored by the API.
    #[arg(long, hide = true, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub start_crawl_date: Option<DateTime<Utc>>,

    /// Deprecated by Exa; ignored by the API.
    #[arg(long, hide = true, value_name = "DATE", value_parser = parse_date, help_heading = "Filters")]
    pub end_crawl_date: Option<DateTime<Utc>>,

    /// Phrase of up to five words that must appear in the page text.
    #[arg(long, value_name = "PHRASE", help_heading = "Filters")]
    pub include_text: Vec<String>,

    /// Phrase of up to five words that must not appear in the page text.
    #[arg(long, value_name = "PHRASE", help_heading = "Filters")]
    pub exclude_text: Vec<String>,

    /// Drop unsafe content.
    #[arg(long)]
    pub moderation: bool,

    /// Extra query phrasing searched alongside the main one; deep types only (repeatable).
    #[arg(long, value_name = "QUERY")]
    pub additional_query: Vec<String>,

    /// Source preferences or novelty and dedup constraints for the synthesized output.
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Two-letter country code to localise results, e.g. US.
    #[arg(long, value_name = "CC")]
    pub user_location: Option<String>,

    /// JSON schema for output.content, inline or @file; root type "object" or "text".
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json, conflicts_with = "structured")]
    pub output_schema: Option<Value>,

    /// Same as --output-schema '{"type":"object"}': Exa chooses the fields.
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
    /// URLs or result ids from `search`, 1..=100.
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
    /// The question.
    pub query: String,

    /// Attach each citation's full page text.
    #[arg(long)]
    pub text: bool,

    /// exa | exa-fast | exa-pro | exa-research [default: exa].
    #[arg(long, value_name = "MODEL", value_parser = parse_enum::<AnswerRequestModel>)]
    pub model: Option<AnswerRequestModel>,

    /// Source preferences or constraints for the answer.
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

    /// Results to return, 1..=100 [default: 10].
    #[arg(short = 'n', long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
    pub num_results: Option<i64>,

    /// Restrict to one kind of page: company | people | publication | news |
    /// "personal site" | "financial report".
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
    /// Start a run. Prints the run object {id, status, ...}; with --wait, the finished run.
    ///
    /// Use for open-ended list building ("find 50 companies that..."), multi-hop research,
    /// enriching many entities across fields, or continuing an earlier run ("10 more").
    /// Prefer `search` when one query answers the question.
    ///
    /// The finished run carries output.content (text, or JSON matching --output-schema),
    /// output.grounding citations, and costDollars. Terminal statuses: completed, failed, cancelled.
    /// Without --wait, poll with `agent get ID`.
    ///
    /// Price per run: minimal $0.012, low $0.025, medium $0.10, high $0.50, xhigh $1.00;
    /// auto (default) meters usage up to $5, max up to $20; --max-cost lowers those caps.
    Run(AgentRunArgs),
    /// Fetch a run: status, and output plus costDollars once it has finished.
    Get {
        /// Run id.
        id: String,
    },
    /// List runs, newest first. Page with --cursor set to nextCursor from the previous page.
    List {
        /// Runs per page, 1..=100.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
        limit: Option<i64>,
        /// nextCursor from the previous page.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Event log of a run: lifecycle, tool calls, progress, errors.
    Events {
        /// Run id.
        id: String,
        /// Events per page, 1..=100.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=100))]
        limit: Option<i64>,
        /// nextCursor from the previous page.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Cancel a queued or running run; its output is discarded.
    Cancel {
        /// Run id.
        id: String,
    },
    /// Stop a running run and keep what it has produced so far.
    Stop {
        /// Run id.
        id: String,
    },
}

/// Arguments for `agent run`.
#[derive(Debug, Args)]
pub struct AgentRunArgs {
    /// What to research or build, including the criteria and how many results you want.
    pub query: String,

    /// Source preferences, exclusions, or output conventions for the agent.
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// minimal | low | medium | high | xhigh: fixed-price tiers, cheapest to most thorough.
    /// auto (default) lets Exa choose, up to $5. max favours completeness over cost, up to $20.
    #[arg(long, value_name = "EFFORT", value_parser = parse_enum::<AgentEffort>)]
    pub effort: Option<AgentEffort>,

    /// Spend cap in USD, 1..=100. Only auto and max are metered; fixed tiers cost their listed price.
    #[arg(long, value_name = "USD", value_parser = parse_max_cost)]
    pub max_cost: Option<f64>,

    /// JSON schema for output.content, inline or @file.
    #[arg(long, value_name = "JSON|@FILE", value_parser = parse_json)]
    pub output_schema: Option<Value>,

    /// Completed run to continue from, e.g. to ask for more results.
    #[arg(long, value_name = "RUN_ID")]
    pub previous_run_id: Option<String>,

    /// Exa Connect provider to enable (repeatable): fiber (B2B people and companies),
    /// `financial_datasets` (US tickers), similarweb (web traffic), baselayer (US business KYB),
    /// affiliate (product catalogs), particle (podcast transcripts), jinko (flights and hotels),
    /// polymarket (prediction markets).
    #[arg(long, value_name = "PROVIDER", value_parser = parse_enum::<AgentDataSourceProvider>)]
    pub data_source: Vec<AgentDataSourceProvider>,

    /// KEY=VALUE stored with the run (repeatable).
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_value)]
    pub metadata: Vec<(String, String)>,

    /// Block until a terminal status and print the finished run instead of the initial one.
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
    /// Endpoint path, e.g. /agent/runs or search.
    pub path: String,

    /// GET or POST.
    #[arg(short = 'X', long, value_name = "VERB", default_value = "POST", value_parser = parse_method)]
    pub method: Method,

    /// Inline JSON body.
    #[arg(long, value_name = "JSON", conflicts_with = "body_file")]
    pub body: Option<String>,

    /// JSON body from a file, or - for stdin.
    #[arg(long, value_name = "FILE")]
    pub body_file: Option<PathBuf>,
}

/// `keys` subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysAction {
    /// Configured keys, masked, with their health. Same as `status`.
    List,
    /// Append keys to config.toml; duplicates are skipped.
    Add {
        /// One or more Exa API keys.
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
    },
    /// Remove a key from config.toml.
    Remove {
        /// Full key, fingerprint, or masked label as shown by `status`.
        #[arg(value_name = "KEY|FINGERPRINT|LABEL")]
        key: String,
    },
    /// Clear exhausted, invalid and cooldown marks so keys are tried again.
    Reset {
        /// Full key, fingerprint, or masked label; omit to reset every key.
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
