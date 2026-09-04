# exa-pool

Command-line client for the [Exa](https://exa.ai) API that spreads requests
over a pool of API keys, round-robin, and remembers which keys are dead.

```
exa-pool keys add <KEY1> <KEY2> ...
exa-pool search "rust http client" -n 5 --type fast --max-characters 500
exa-pool search "rust http client" --type deep --structured --highlights-query "which crate"
exa-pool search "acme corp" --category company --highlights --include-text "series B"
exa-pool contents https://example.com --summary-query "pricing" --subpages 2 --subpage-target docs
exa-pool answer "what is retrieval-augmented generation?" --text
exa-pool find-similar https://exa.ai -n 3 --exclude-source-domain
exa-pool agent run "list Rust HTTP client crates with their maintainers" --effort low --wait
exa-pool agent get <RUN_ID>          # also: list, events, cancel, stop
exa-pool raw /agent/runs -X GET       # any endpoint, GET or POST
exa-pool status
```

Responses are printed as JSON on stdout (`--compact` for one line).
Per-attempt diagnostics go to stderr (`-q` to silence).

## Install

Prebuilt binaries for Linux (x86_64, aarch64, static musl), macOS (Intel,
Apple Silicon) and Windows (x86_64) are attached to each
[GitHub release](https://github.com/trancong12102/exa-pool/releases), with a
`.sha256` next to every archive. Or build from source:

```
cargo install --git https://github.com/trancong12102/exa-pool --locked
```

## Rotation policy

Exa reports billing and auth problems explicitly, so the pool never has to
guess. Decisions are keyed on the HTTP status first and the Exa error `tag`
second (see <https://exa.ai/docs/reference/error-codes>):

| Response | Verdict | What happens to the key |
|---|---|---|
| 2xx | success | failure counter reset, `costDollars.total` added to its spend |
| 401 `INVALID_API_KEY` | invalid | marked **invalid** on the first hit, skipped until `keys reset` |
| 402 `NO_MORE_CREDITS`, `API_KEY_BUDGET_EXCEEDED`, `TEAM_BUDGET_EXCEEDED` | exhausted | marked **exhausted** on the first hit, skipped until `keys reset` |
| 429 | rate limited | cooldown for `Retry-After` (or `rate_limit_cooldown_ms`), next key tried immediately; never counts as a strike |
| 5xx, timeout, connection error | transient | failure counter +1; after `max_consecutive_failures` in a row the key is quarantined for `quarantine_seconds` (still *active*, never labelled exhausted) |
| 400, 403, 404, 422, 501, 504 `CRAWL_*` | request error | nothing; the CLI stops with exit code 3 because another key would fail the same way |

Selection is round-robin over eligible keys (active and not cooling down).
The cursor is persisted so consecutive CLI invocations keep rotating. If every
remaining key is cooling down, the CLI sleeps until the earliest one frees,
up to `max_wait_ms`, then gives up.

Each invocation makes at most `max_attempts` HTTP calls in total.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | config, state, or input problem |
| 2 | usage error (clap) |
| 3 | Exa rejected the request (4xx that is not key-related) |
| 4 | no usable key: all exhausted/invalid, or cooling longer than `max_wait_ms` |
| 5 | transient failures exceeded `max_attempts` |

## Files

Everything lives in one directory, chosen in this order: `--home`,
`$EXA_POOL_HOME`, `$XDG_CONFIG_HOME/exa-pool`, `~/.config/exa-pool`.

- `config.toml` (mode 0600), keys and tunables
- `state.json` (mode 0600), per-key health, spend, round-robin cursor
- `state.lock`, an advisory lock. Parallel invocations read and advance the
  cursor one at a time, so they walk the pool in order instead of all grabbing
  the same key, and no two writers can truncate the state file. With more
  concurrent calls than keys, keys repeat by design.

Keys are also read from `EXA_API_KEYS` (comma/newline separated), appended
after the file's keys. The single-key `EXA_API_KEY` variable is ignored on
purpose, so a key exported for other tools never joins the pool by accident. Keys are never printed in
full; `status` shows a masked label and a fingerprint.

`config.toml` with defaults:

```toml
base_url = "https://api.exa.ai"
timeout_seconds = 90
max_attempts = 6
max_consecutive_failures = 3
quarantine_seconds = 300
rate_limit_cooldown_ms = 1000
max_wait_ms = 10000
keys = []
```

## Key management

```
exa-pool keys list                # same as `status`
exa-pool keys add KEY...
exa-pool keys remove <key|fingerprint|label>
exa-pool keys reset [key|fingerprint|label]   # clear exhausted/invalid/cooldown
```

Run `keys reset` after topping up an account.

## Agent runs

`agent run` posts to `/agent/runs` and prints the run object with its id.
Runs execute asynchronously; `--wait` polls `GET /agent/runs/{id}` every
`--poll-interval` seconds (default 3) until the status is `completed`,
`failed`, or `cancelled`, then prints the final object. The id is written to
stderr as soon as it exists, so a Ctrl-C during `--wait` still leaves you
something to pass to `agent get` or `agent cancel`.

`--effort` takes the spec enum (`minimal`, `low`, `medium`, `high`, `xhigh`,
`auto`, `max`). The server default is `auto`, which Exa caps at $5 per run;
`max` is capped at $20. `--max-cost` sets your own cap for those two tiers.
`--output-schema` accepts inline JSON or `@file`, `--data-source` enables an
Exa Connect provider, `--metadata KEY=VALUE` is stored with the run.

Spend accounting: `AgentRun` carries the run's cumulative cost on every
read, so polls and `agent get` do not add to a key's `SPENT`. `agent run
--wait` makes one billed read after the run settles, so a waited run is
counted exactly once. Runs started without `--wait` are never counted.

## Parity with exa-mcp-server

Every request field the live tools of Exa's own
[MCP server](https://github.com/exa-labs/exa-mcp-server) send is a flag here:
the advanced-search filters (`--start-crawl-date`, `--end-crawl-date`,
`--include-text`, `--exclude-text`, `--moderation`, `--user-location`), the
contents options (`--context`, `--highlights-query`, `--highlights-num-sentences`,
`--highlights-per-url`, `--summary-query`, `--max-age-hours`,
`--livecrawl-timeout`, `--subpages`, `--subpage-target`), structured output
(`--output-schema`, `--structured`), and agent runs. The MCP's deprecated
presets (company research, people and LinkedIn search, code context, deep
search) are one or two of those flags on `search`, for example
`--category company --highlights` or `--type deep --structured`, and are not
separate subcommands. The deprecated `/research/v1` endpoint is not wrapped;
its docs page is empty and the Agent API replaces it.

`--include-text` and `--exclude-text` are the only search fields not backed
by a generated type: the published spec omits them, while Exa's SDK still
documents them (one phrase of up to five words). They are appended by a small
wrapper in `src/exa.rs`, like `urls` for `/contents`.

## Known limits

- Exa documents 10 QPS on `/search` and `/answer`, 100 QPS on `/contents`,
  but not whether that is per key or per account. If all your keys belong to
  one account, rotating on 429 will not raise your throughput.
- `NO_MORE_CREDITS` is account-scoped. Sibling keys on the same account each
  burn one failed request before they are marked exhausted.

## Generated types

Request bodies are built from Rust types generated out of Exa's own OpenAPI
3.1 document, so the CLI cannot send a field or enum value the spec does not
know. `--type neural`, for example, is rejected locally with the list the
spec allows (`instant`, `fast`, `auto`, `deep-lite`, `deep`, `deep-reasoning`).

- `spec/exa-spec.json` is the vendored copy of <https://exa.ai/docs/exa-spec.json>.
- `src/generated/` is produced by [openapi-to-rust](https://github.com/gpu-cli/openapi-to-rust)
  in types-only mode and is committed. Do not edit it.
- `just spec-update` downloads the latest spec, `just codegen` regenerates,
  `just codegen-check` (part of `just ci`) fails when the two drift.
- `just spec-check` fails when exa.ai serves a spec that differs from the
  vendored one; `just spec-sync` pulls it, regenerates, and runs `cargo check`.
  `.github/workflows/spec-sync.yml` does the same every Monday and opens a PR
  when anything changed. Exa currently publishes no changelog for the spec, so
  review the diff in `src/generated/` and fix `src/exa.rs` / `src/cli.rs` by hand
  if a field was renamed or removed.
- The generator version is pinned in the justfile and installed on demand.

One known gap: the spec models `/contents` as `allOf` of `ContentsOptions`
and a `oneOf` requiring either `urls` or `ids`. The generator keeps the
options but drops the `oneOf`, so `urls` is added by a small wrapper in
`src/exa.rs`. Together with `includeText`/`excludeText` on `/search` those
are the only hand-written request fields; everything else, including the
enums above, comes from the spec unchanged. JSON schemas passed with
`--output-schema` and `--summary-schema` are validated against the spec's
schema shape before anything is sent.

## Development

```
just            # fmt-check, codegen-check, clippy -D warnings, nextest, doc, deny, audit, machete
just fmt
just test
lefthook install   # pre-commit: fmt + clippy + machete; pre-push: just ci
```

`ci.yml` runs `just ci` on every push to `main` and every pull request.
`spec-sync.yml` pulls the Exa spec weekly and opens a PR when it changed.

Releases are driven by [Conventional Commits](https://www.conventionalcommits.org/):
`feat:` bumps the minor version, `fix:` and `perf:` bump the patch, a `!` or a
`BREAKING CHANGE:` footer bumps the major. Other types (`docs:`, `ci:`,
`chore:`, `refactor:`, `test:`, `build:`) never trigger a release. The
`commit-msg` hook in `lefthook.yml` rejects messages that do not fit.

On every push to `main`, `release.yml` runs release-please, which keeps a
"chore(main): release X.Y.Z" pull request up to date with the version bump in
`Cargo.toml` and `Cargo.lock` and the generated `CHANGELOG.md`. Merging that
PR creates the tag and the GitHub release, and the same workflow then attaches
one archive per target from the matrix in that file. Nothing is tagged by hand.

Lints are configured in `Cargo.toml` (`clippy::all`, `pedantic`, `nursery`,
`cargo`, plus a hand-picked set of restriction lints such as `unwrap_used`,
`panic`, `indexing_slicing`, `arithmetic_side_effects`, `as_conversions`),
with `unsafe_code = "forbid"`. `clippy.toml` relaxes the restriction lints
inside tests only. `deny.toml` enforces a license allowlist, bans OpenSSL and
native-tls, and checks RustSec advisories.

The rotation policy is pure (`src/policy.rs`) and the pool is exercised in
tests through a scripted fake transport and fake clock (`src/pool.rs`), so
every row of the table above has a test that runs without a network.
