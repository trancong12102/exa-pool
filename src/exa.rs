//! Request builders for the Exa endpoints the CLI exposes.
//!
//! Bodies are built from the types in [`crate::generated`], which come
//! straight from Exa's `OpenAPI` document, so a field or enum value that the
//! spec does not know cannot be sent. The two exceptions are documented on
//! `SearchBody` and `ContentsBody`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::generated::{
    AgentBudget, AgentDataSource, AgentDataSourceProvider, AgentEffort, AnswerRequest,
    AnswerRequestModel, ContentsOptions, ContentsOptionsContext, ContentsOptionsContextVariant2,
    ContentsOptionsExtras, ContentsOptionsHighlights, ContentsOptionsHighlightsVariant2,
    ContentsOptionsLivecrawl, ContentsOptionsSubpageTarget, ContentsOptionsSummary,
    ContentsOptionsSummarySchema, ContentsOptionsText, ContentsOptionsTextVariant2,
    CreateAgentRunRequest, CreateAgentRunRequestMetadata, CreateAgentRunRequestOutputSchema,
    FindSimilarRequest, FindSimilarRequestCategory, SearchRequest, SearchRequestCategory,
    SearchRequestOutputSchema, SearchRequestTypeInstant,
};
use crate::transport::Method;

/// A prepared call: verb, path relative to the API origin, and JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// HTTP verb.
    pub method: Method,
    /// Path such as `/search` (leading slash included, query string allowed).
    pub path: String,
    /// JSON body; ignored for GET.
    pub body: Value,
    /// Whether `costDollars.total` in the response counts toward the key's
    /// spend. Agent polling reports cumulative cost on every read, so those
    /// calls do not bill.
    pub bills: bool,
}

impl Request {
    /// Full URL for `base_url`.
    #[must_use]
    pub fn url(&self, base_url: &str) -> String {
        format!("{}{}", base_url.trim_end_matches('/'), self.path)
    }

    fn typed<T: Serialize>(path: &str, body: &T) -> Result<Self> {
        let body = serde_json::to_value(body).map_err(|e| Error::Input(e.to_string()))?;
        Ok(Self {
            method: Method::Post,
            path: path.to_owned(),
            body,
            bills: true,
        })
    }

    const fn get(path: String) -> Self {
        Self {
            method: Method::Get,
            path,
            body: Value::Null,
            bills: false,
        }
    }
}

/// Which page contents to attach to results.
///
/// Every field maps onto [`ContentsOptions`]; a highlight, summary, or
/// context sub-option switches that block from `true` to its object form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent on/off content option, not a state machine"
)]
pub struct ContentsChoice {
    /// Include full page text.
    pub text: bool,
    /// Cap on text characters per result (implies `text`).
    pub max_characters: Option<i64>,
    /// Include highlight snippets.
    pub highlights: bool,
    /// Query the highlights should answer (implies `highlights`).
    pub highlights_query: Option<String>,
    /// Sentences per highlight (implies `highlights`).
    pub highlights_num_sentences: Option<i64>,
    /// Highlights per result (implies `highlights`).
    pub highlights_per_url: Option<i64>,
    /// Cap on highlight characters per result (implies `highlights`).
    pub highlights_max_characters: Option<i64>,
    /// Include an LLM summary.
    pub summary: bool,
    /// Focus query for the summary (implies `summary`).
    pub summary_query: Option<String>,
    /// JSON schema the summary must follow (implies `summary`).
    pub summary_schema: Option<Value>,
    /// Include a single LLM-ready context string built from all results.
    pub context: bool,
    /// Cap on context characters (implies `context`).
    pub context_max_characters: Option<i64>,
    /// Livecrawl mode.
    pub livecrawl: Option<ContentsOptionsLivecrawl>,
    /// Livecrawl timeout in milliseconds.
    pub livecrawl_timeout: Option<i64>,
    /// Serve cached content no older than this many hours.
    pub max_age_hours: Option<i64>,
    /// Number of subpages to crawl per result.
    pub subpages: Option<i64>,
    /// Keywords that pick which subpages to crawl.
    pub subpage_target: Vec<String>,
    /// Number of outbound links to return per result.
    pub extras_links: Option<i64>,
    /// Number of image links to return per result.
    pub extras_image_links: Option<i64>,
}

impl ContentsChoice {
    const fn wants_text(&self) -> bool {
        self.text || self.max_characters.is_some()
    }

    const fn wants_highlights(&self) -> bool {
        self.highlights
            || self.highlights_query.is_some()
            || self.highlights_num_sentences.is_some()
            || self.highlights_per_url.is_some()
            || self.highlights_max_characters.is_some()
    }

    const fn wants_summary(&self) -> bool {
        self.summary || self.summary_query.is_some() || self.summary_schema.is_some()
    }

    const fn wants_context(&self) -> bool {
        self.context || self.context_max_characters.is_some()
    }

    const fn wants_extras(&self) -> bool {
        self.extras_links.is_some() || self.extras_image_links.is_some()
    }

    /// True when at least one option was requested.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !(self.wants_text()
            || self.wants_highlights()
            || self.wants_summary()
            || self.wants_context()
            || self.wants_extras()
            || self.livecrawl.is_some()
            || self.livecrawl_timeout.is_some()
            || self.max_age_hours.is_some()
            || self.subpages.is_some()
            || !self.subpage_target.is_empty())
    }

    /// Spec-typed contents block, or `None` when nothing was requested.
    ///
    /// # Errors
    /// When `summary_schema` is not a schema the spec accepts.
    pub fn to_options(&self) -> Result<Option<ContentsOptions>> {
        if self.is_empty() {
            return Ok(None);
        }
        let text = self.wants_text().then(|| match self.max_characters {
            Some(n) => {
                ContentsOptionsText::ContentsOptionsTextVariant2(ContentsOptionsTextVariant2 {
                    max_characters: Some(Some(n)),
                    ..ContentsOptionsTextVariant2::default()
                })
            }
            None => ContentsOptionsText::Boolean(true),
        });
        let highlights = self.wants_highlights().then(|| {
            let detailed = self.highlights_query.is_some()
                || self.highlights_num_sentences.is_some()
                || self.highlights_per_url.is_some()
                || self.highlights_max_characters.is_some();
            if detailed {
                ContentsOptionsHighlights::ContentsOptionsHighlightsVariant2(
                    ContentsOptionsHighlightsVariant2 {
                        query: self.highlights_query.clone().map(Some),
                        num_sentences: self.highlights_num_sentences.map(Some),
                        highlights_per_url: self.highlights_per_url.map(Some),
                        max_characters: self.highlights_max_characters.map(Some),
                        ..ContentsOptionsHighlightsVariant2::default()
                    },
                )
            } else {
                ContentsOptionsHighlights::Boolean(true)
            }
        });
        let summary = if self.wants_summary() {
            let schema = self
                .summary_schema
                .clone()
                .map(|v| {
                    serde_json::from_value::<ContentsOptionsSummarySchema>(v)
                        .map_err(|e| Error::Input(format!("summary schema: {e}")))
                })
                .transpose()?;
            Some(ContentsOptionsSummary {
                query: self.summary_query.clone().map(Some),
                schema: schema.map(Some),
            })
        } else {
            None
        };
        let context = self
            .wants_context()
            .then_some(match self.context_max_characters {
                Some(n) => ContentsOptionsContext::ContentsOptionsContextVariant2(
                    ContentsOptionsContextVariant2 {
                        max_characters: Some(n),
                    },
                ),
                None => ContentsOptionsContext::Boolean(true),
            });
        let extras = self.wants_extras().then(|| ContentsOptionsExtras {
            links: self.extras_links.map(Some),
            image_links: self.extras_image_links.map(Some),
            ..ContentsOptionsExtras::default()
        });
        let subpage_target = (!self.subpage_target.is_empty())
            .then(|| ContentsOptionsSubpageTarget::StringArray(self.subpage_target.clone()));
        Ok(Some(ContentsOptions {
            text: text.map(Some),
            highlights: highlights.map(Some),
            summary: summary.map(Some),
            context: context.map(Some),
            extras: extras.map(Some),
            livecrawl: self.livecrawl.clone().map(Some),
            livecrawl_timeout: self.livecrawl_timeout.map(Some),
            max_age_hours: self.max_age_hours.map(Some),
            subpages: self.subpages.map(Some),
            subpage_target: subpage_target.map(Some),
        }))
    }
}

/// Parameters for `/search`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchParams {
    /// Natural-language query.
    pub query: String,
    /// Search type (spec enum).
    pub search_type: Option<SearchRequestTypeInstant>,
    /// Category filter (spec enum).
    pub category: Option<SearchRequestCategory>,
    /// Result count.
    pub num_results: Option<i64>,
    /// Restrict to these domains.
    pub include_domains: Vec<String>,
    /// Drop these domains.
    pub exclude_domains: Vec<String>,
    /// Lower bound on publish date.
    pub start_published_date: Option<DateTime<Utc>>,
    /// Upper bound on publish date.
    pub end_published_date: Option<DateTime<Utc>>,
    /// Lower bound on crawl date.
    pub start_crawl_date: Option<DateTime<Utc>>,
    /// Upper bound on crawl date.
    pub end_crawl_date: Option<DateTime<Utc>>,
    /// Phrases that must appear in page text (not in the spec; see `SearchBody`).
    pub include_text: Vec<String>,
    /// Phrases that must not appear in page text (not in the spec; see `SearchBody`).
    pub exclude_text: Vec<String>,
    /// Extra query variations.
    pub additional_queries: Vec<String>,
    /// Behaviour guidance for the request.
    pub system_prompt: Option<String>,
    /// Two-letter ISO country code.
    pub user_location: Option<String>,
    /// Drop results that fail moderation.
    pub moderation: Option<bool>,
    /// JSON schema for structured output (deep search types).
    pub output_schema: Option<Value>,
    /// Attached contents.
    pub contents: ContentsChoice,
}

/// Parameters for `/contents`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentsParams {
    /// URLs or Exa result ids.
    pub urls: Vec<String>,
    /// What to fetch.
    pub contents: ContentsChoice,
}

/// Parameters for `/answer`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnswerParams {
    /// Question.
    pub query: String,
    /// Include citation text.
    pub text: bool,
    /// Model (spec enum).
    pub model: Option<AnswerRequestModel>,
    /// Behaviour guidance for the request.
    pub system_prompt: Option<String>,
}

/// Parameters for `/findSimilar`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindSimilarParams {
    /// Seed URL.
    pub url: String,
    /// Result count.
    pub num_results: Option<i64>,
    /// Category filter (spec enum).
    pub category: Option<FindSimilarRequestCategory>,
    /// Restrict to these domains.
    pub include_domains: Vec<String>,
    /// Drop these domains.
    pub exclude_domains: Vec<String>,
    /// Omit results from the seed's own domain.
    pub exclude_source_domain: bool,
    /// Lower bound on publish date.
    pub start_published_date: Option<DateTime<Utc>>,
    /// Upper bound on publish date.
    pub end_published_date: Option<DateTime<Utc>>,
    /// Attached contents.
    pub contents: ContentsChoice,
}

/// Parameters for `POST /agent/runs`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentRunParams {
    /// Task for the agent.
    pub query: String,
    /// Behaviour guidance.
    pub system_prompt: Option<String>,
    /// Effort tier (spec enum).
    pub effort: Option<AgentEffort>,
    /// Spend cap in USD for the metered tiers.
    pub max_cost_dollars: Option<f64>,
    /// JSON schema the structured output must follow.
    pub output_schema: Option<Value>,
    /// Continue from an earlier run.
    pub previous_run_id: Option<String>,
    /// Exa Connect providers to enable (spec enum).
    pub data_sources: Vec<AgentDataSourceProvider>,
    /// Caller metadata stored with the run.
    pub metadata: Vec<(String, String)>,
}

/// `/search` body. Exa's SDK and MCP server still send `includeText` and
/// `excludeText`, which the current spec omits; they are the only search
/// fields not backed by a generated type.
#[derive(Debug, Serialize)]
struct SearchBody {
    #[serde(flatten)]
    spec: SearchRequest,
    #[serde(rename = "includeText", skip_serializing_if = "Vec::is_empty")]
    include_text: Vec<String>,
    #[serde(rename = "excludeText", skip_serializing_if = "Vec::is_empty")]
    exclude_text: Vec<String>,
}

/// Build a `/search` call.
///
/// # Errors
/// When `output_schema` or `summary_schema` is not a shape the spec accepts.
pub fn search(p: &SearchParams) -> Result<Request> {
    let mut b = SearchRequest::builder(p.query.clone());
    if let Some(t) = &p.search_type {
        b = b.r#type(t.clone());
    }
    if let Some(c) = &p.category {
        b = b.category(c.clone());
    }
    if let Some(n) = p.num_results {
        b = b.num_results(n);
    }
    if !p.include_domains.is_empty() {
        b = b.include_domains(p.include_domains.clone());
    }
    if !p.exclude_domains.is_empty() {
        b = b.exclude_domains(p.exclude_domains.clone());
    }
    if let Some(d) = p.start_published_date {
        b = b.start_published_date(d);
    }
    if let Some(d) = p.end_published_date {
        b = b.end_published_date(d);
    }
    if let Some(d) = p.start_crawl_date {
        b = b.start_crawl_date(d);
    }
    if let Some(d) = p.end_crawl_date {
        b = b.end_crawl_date(d);
    }
    if !p.additional_queries.is_empty() {
        b = b.additional_queries(p.additional_queries.clone());
    }
    if let Some(s) = &p.system_prompt {
        b = b.system_prompt(s.clone());
    }
    if let Some(l) = &p.user_location {
        b = b.user_location(l.clone());
    }
    if let Some(m) = p.moderation {
        b = b.moderation(m);
    }
    if let Some(schema) = &p.output_schema {
        let schema = serde_json::from_value::<SearchRequestOutputSchema>(schema.clone())
            .map_err(|e| Error::Input(format!("output schema: {e}")))?;
        b = b.output_schema(schema);
    }
    if let Some(c) = p.contents.to_options()? {
        b = b.contents(c);
    }
    Request::typed(
        "/search",
        &SearchBody {
            spec: b.build(),
            include_text: p.include_text.clone(),
            exclude_text: p.exclude_text.clone(),
        },
    )
}

/// `/contents` body. The spec models `urls`/`ids` as a `oneOf` of required
/// keys merged with [`ContentsOptions`] through `allOf`; the generator drops
/// the `oneOf` half, so the URL list is added here and the options flattened.
#[derive(Debug, Serialize)]
struct ContentsBody {
    urls: Vec<String>,
    #[serde(flatten)]
    options: ContentsOptions,
}

/// Build a `/contents` call. Defaults to full text when nothing is chosen.
///
/// # Errors
/// When `summary_schema` is not a shape the spec accepts.
pub fn contents(p: &ContentsParams) -> Result<Request> {
    let mut choice = p.contents.clone();
    if choice.is_empty() {
        choice.text = true;
    }
    let options = choice.to_options()?.unwrap_or_default();
    Request::typed(
        "/contents",
        &ContentsBody {
            urls: p.urls.clone(),
            options,
        },
    )
}

/// Build an `/answer` call (non-streaming).
///
/// # Errors
/// Only if the typed body cannot be serialised, which would be a bug.
pub fn answer(p: &AnswerParams) -> Result<Request> {
    let mut b = AnswerRequest::builder(p.query.clone())
        .stream(false)
        .text(p.text);
    if let Some(m) = &p.model {
        b = b.model(m.clone());
    }
    if let Some(s) = &p.system_prompt {
        b = b.system_prompt(s.clone());
    }
    Request::typed("/answer", &b.build())
}

/// Build a `/findSimilar` call.
///
/// # Errors
/// When `summary_schema` is not a shape the spec accepts.
pub fn find_similar(p: &FindSimilarParams) -> Result<Request> {
    let mut b = FindSimilarRequest::builder(p.url.clone());
    if let Some(n) = p.num_results {
        b = b.num_results(n);
    }
    if let Some(c) = &p.category {
        b = b.category(c.clone());
    }
    if !p.include_domains.is_empty() {
        b = b.include_domains(p.include_domains.clone());
    }
    if !p.exclude_domains.is_empty() {
        b = b.exclude_domains(p.exclude_domains.clone());
    }
    if p.exclude_source_domain {
        b = b.exclude_source_domain(true);
    }
    if let Some(d) = p.start_published_date {
        b = b.start_published_date(d);
    }
    if let Some(d) = p.end_published_date {
        b = b.end_published_date(d);
    }
    if let Some(c) = p.contents.to_options()? {
        b = b.contents(c);
    }
    Request::typed("/findSimilar", &b.build())
}

/// Build a `POST /agent/runs` call. The response is the run object with its
/// id; the run itself executes asynchronously.
///
/// # Errors
/// When `output_schema` is not a JSON object.
pub fn agent_run(p: &AgentRunParams) -> Result<Request> {
    let mut b = CreateAgentRunRequest::builder(p.query.clone());
    if let Some(s) = &p.system_prompt {
        b = b.system_prompt(s.clone());
    }
    if let Some(e) = &p.effort {
        b = b.effort(e.clone());
    }
    if let Some(cap) = p.max_cost_dollars {
        b = b.budget(AgentBudget {
            max_cost_dollars: Some(cap),
        });
    }
    if let Some(id) = &p.previous_run_id {
        b = b.previous_run_id(id.clone());
    }
    if !p.data_sources.is_empty() {
        b = b.data_sources(
            p.data_sources
                .iter()
                .map(|provider| AgentDataSource {
                    provider: provider.clone(),
                })
                .collect(),
        );
    }
    if !p.metadata.is_empty() {
        b = b.metadata(CreateAgentRunRequestMetadata {
            additional_properties: p.metadata.iter().cloned().collect(),
        });
    }
    let mut req = b.build();
    if let Some(schema) = &p.output_schema {
        let schema = serde_json::from_value::<CreateAgentRunRequestOutputSchema>(schema.clone())
            .map_err(|e| Error::Input(format!("output schema: {e}")))?;
        req.output_schema = Some(Some(schema));
    }
    let mut request = Request::typed("/agent/runs", &req)?;
    request.bills = false;
    Ok(request)
}

/// `GET /agent/runs/{id}`.
#[must_use]
pub fn agent_get(id: &str) -> Request {
    Request::get(format!("/agent/runs/{}", encode(id)))
}

/// `GET /agent/runs?limit=&cursor=`.
#[must_use]
pub fn agent_list(limit: Option<i64>, cursor: Option<&str>) -> Request {
    Request::get(format!("/agent/runs{}", query(limit, cursor)))
}

/// `GET /agent/runs/{id}/events?limit=&cursor=`.
#[must_use]
pub fn agent_events(id: &str, limit: Option<i64>, cursor: Option<&str>) -> Request {
    Request::get(format!(
        "/agent/runs/{}/events{}",
        encode(id),
        query(limit, cursor)
    ))
}

/// `POST /agent/runs/{id}/cancel`.
#[must_use]
pub fn agent_cancel(id: &str) -> Request {
    Request {
        method: Method::Post,
        path: format!("/agent/runs/{}/cancel", encode(id)),
        body: Value::Object(serde_json::Map::new()),
        bills: false,
    }
}

/// `POST /agent/runs/{id}/stop`.
#[must_use]
pub fn agent_stop(id: &str) -> Request {
    Request {
        method: Method::Post,
        path: format!("/agent/runs/{}/stop", encode(id)),
        body: Value::Object(serde_json::Map::new()),
        bills: false,
    }
}

fn query(limit: Option<i64>, cursor: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(n) = limit {
        parts.push(format!("limit={n}"));
    }
    if let Some(c) = cursor {
        parts.push(format!("cursor={}", encode(c)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Percent-encode a path or query component.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0x0f));
            }
        }
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16)
        .unwrap_or('0')
        .to_ascii_uppercase()
}

/// Pass an arbitrary path and body straight through.
#[must_use]
pub fn raw(method: Method, path: &str, body: Value) -> Request {
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    Request {
        method,
        path,
        body,
        bills: true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn search_omits_empty_fields() {
        let req = search(&SearchParams {
            query: "q".into(),
            num_results: Some(3),
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(req.path, "/search");
        assert_eq!(req.method, Method::Post);
        assert!(req.bills);
        assert_eq!(req.body, json!({ "query": "q", "numResults": 3 }));
    }

    #[test]
    fn search_uses_spec_enums_and_nests_contents() {
        let req = search(&SearchParams {
            query: "q".into(),
            search_type: Some(SearchRequestTypeInstant::DeepLite),
            category: Some(SearchRequestCategory::PersonalSite),
            include_domains: vec!["a.com".into()],
            start_published_date: Some("2024-01-02T00:00:00Z".parse().unwrap()),
            contents: ContentsChoice {
                max_characters: Some(500),
                highlights: true,
                livecrawl: Some(ContentsOptionsLivecrawl::Fallback),
                ..ContentsChoice::default()
            },
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(
            req.body,
            json!({
                "query": "q",
                "type": "deep-lite",
                "category": "personal site",
                "includeDomains": ["a.com"],
                "startPublishedDate": "2024-01-02T00:00:00Z",
                "contents": {
                    "text": { "maxCharacters": 500 },
                    "highlights": true,
                    "livecrawl": "fallback"
                }
            })
        );
    }

    #[test]
    fn search_advanced_fields_match_mcp_shapes() {
        let req = search(&SearchParams {
            query: "q".into(),
            start_crawl_date: Some("2024-01-01T00:00:00Z".parse().unwrap()),
            moderation: Some(true),
            include_text: vec!["rust".into()],
            exclude_text: vec!["java".into()],
            output_schema: Some(json!({ "type": "object", "properties": {} })),
            contents: ContentsChoice {
                text: true,
                highlights_query: Some("why".into()),
                highlights_num_sentences: Some(2),
                highlights_per_url: Some(3),
                summary_query: Some("tl;dr".into()),
                context_max_characters: Some(9000),
                max_age_hours: Some(24),
                livecrawl_timeout: Some(5000),
                subpages: Some(2),
                subpage_target: vec!["docs".into(), "pricing".into()],
                extras_links: Some(5),
                ..ContentsChoice::default()
            },
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(
            req.body,
            json!({
                "query": "q",
                "startCrawlDate": "2024-01-01T00:00:00Z",
                "moderation": true,
                "includeText": ["rust"],
                "excludeText": ["java"],
                "outputSchema": { "type": "object", "properties": {} },
                "contents": {
                    "text": true,
                    "highlights": { "query": "why", "numSentences": 2, "highlightsPerUrl": 3 },
                    "summary": { "query": "tl;dr" },
                    "context": { "maxCharacters": 9000 },
                    "maxAgeHours": 24,
                    "livecrawlTimeout": 5000,
                    "subpages": 2,
                    "subpageTarget": ["docs", "pricing"],
                    "extras": { "links": 5 }
                }
            })
        );
        // Everything except the two documented extras still round-trips
        // through the generated type.
        let mut spec_only = req.body;
        spec_only.as_object_mut().unwrap().remove("includeText");
        spec_only.as_object_mut().unwrap().remove("excludeText");
        let parsed: SearchRequest = serde_json::from_value(spec_only).unwrap();
        assert!(matches!(parsed.moderation, Some(Some(true))));
    }

    #[test]
    fn search_accepts_structured_shorthand_and_user_schemas() {
        // `--structured` shorthand: an object schema with no properties.
        let req = search(&SearchParams {
            query: "q".into(),
            output_schema: Some(json!({ "type": "object" })),
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(req.body["outputSchema"], json!({ "type": "object" }));

        // A realistic user schema with nested properties and `required`.
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "year": { "type": "integer" },
                "founder": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                }
            },
            "required": ["name"]
        });
        let req = search(&SearchParams {
            query: "q".into(),
            output_schema: Some(schema.clone()),
            ..SearchParams::default()
        })
        .unwrap();
        assert_eq!(req.body["outputSchema"], schema);
    }

    #[test]
    fn search_rejects_output_schema_outside_spec() {
        let err = search(&SearchParams {
            query: "q".into(),
            output_schema: Some(json!({ "type": "array" })),
            ..SearchParams::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("output schema"), "{err}");
    }

    #[test]
    fn search_round_trips_through_generated_type() {
        let req = search(&SearchParams {
            query: "q".into(),
            search_type: Some(SearchRequestTypeInstant::Fast),
            contents: ContentsChoice {
                summary: true,
                ..ContentsChoice::default()
            },
            ..SearchParams::default()
        })
        .unwrap();
        let parsed: SearchRequest = serde_json::from_value(req.body).unwrap();
        assert_eq!(parsed.query, "q");
        assert!(matches!(
            parsed.r#type,
            Some(Some(SearchRequestTypeInstant::Fast))
        ));
        assert!(
            parsed
                .contents
                .flatten()
                .and_then(|c| c.summary.flatten())
                .is_some()
        );
    }

    #[test]
    fn contents_defaults_to_text_and_is_flat() {
        let req = contents(&ContentsParams {
            urls: vec!["https://x".into()],
            contents: ContentsChoice::default(),
        })
        .unwrap();
        assert_eq!(req.body, json!({ "urls": ["https://x"], "text": true }));
    }

    #[test]
    fn answer_is_not_streamed() {
        let req = answer(&AnswerParams {
            query: "why".into(),
            text: true,
            model: Some(AnswerRequestModel::ExaFast),
            system_prompt: None,
        })
        .unwrap();
        assert_eq!(
            req.body,
            json!({ "query": "why", "stream": false, "text": true, "model": "exa-fast" })
        );
    }

    #[test]
    fn find_similar_flags() {
        let req = find_similar(&FindSimilarParams {
            url: "https://x".into(),
            exclude_source_domain: true,
            ..FindSimilarParams::default()
        })
        .unwrap();
        assert_eq!(req.path, "/findSimilar");
        assert_eq!(
            req.body,
            json!({ "url": "https://x", "excludeSourceDomain": true })
        );
    }

    #[test]
    fn agent_run_body_and_billing() {
        let req = agent_run(&AgentRunParams {
            query: "find rust http clients".into(),
            effort: Some(AgentEffort::Minimal),
            max_cost_dollars: Some(2.5),
            output_schema: Some(
                json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
            ),
            data_sources: vec![AgentDataSourceProvider::Similarweb],
            metadata: vec![("ticket".into(), "42".into())],
            ..AgentRunParams::default()
        })
        .unwrap();
        assert_eq!(req.path, "/agent/runs");
        assert!(!req.bills);
        assert_eq!(
            req.body,
            json!({
                "query": "find rust http clients",
                "effort": "minimal",
                "budget": { "maxCostDollars": 2.5 },
                "outputSchema": { "type": "object", "properties": { "name": { "type": "string" } } },
                "dataSources": [{ "provider": "similarweb" }],
                "metadata": { "ticket": "42" }
            })
        );
        let parsed: CreateAgentRunRequest = serde_json::from_value(req.body).unwrap();
        assert_eq!(parsed.effort, Some(AgentEffort::Minimal));
    }

    #[test]
    fn agent_reads_are_get_and_encoded() {
        let get = agent_get("run_1/x");
        assert_eq!(get.method, Method::Get);
        assert_eq!(get.path, "/agent/runs/run_1%2Fx");
        assert!(!get.bills);
        assert_eq!(
            agent_list(Some(5), Some("a b")).path,
            "/agent/runs?limit=5&cursor=a%20b"
        );
        assert_eq!(agent_events("r", None, None).path, "/agent/runs/r/events");
        assert_eq!(agent_cancel("r").path, "/agent/runs/r/cancel");
        assert_eq!(agent_cancel("r").method, Method::Post);
    }

    #[test]
    fn raw_normalises_slash_and_url() {
        let req = raw(Method::Post, "agent/runs", json!({}));
        assert_eq!(req.path, "/agent/runs");
        assert_eq!(
            req.url("https://api.exa.ai/"),
            "https://api.exa.ai/agent/runs"
        );
    }
}
