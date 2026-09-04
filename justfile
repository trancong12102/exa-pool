set shell := ["sh", "-cu"]

o2r_version := "0.15.0"
spec := "spec/exa-spec.json"
spec_url := "https://exa.ai/docs/exa-spec.json"
gen_args := "--output-dir src/generated --module-name exa_api --types-only --quiet"

default: ci

# Format all sources
fmt:
    cargo fmt --all

# Check formatting without writing
fmt-check:
    cargo fmt --all -- --check

# Clippy with every warning promoted to an error
lint:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Type-check quickly
check:
    cargo check --all-targets --all-features --locked

# Run the test suite
test:
    cargo nextest run --all-features --locked

# Doc build with broken intra-doc links as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked

# Supply-chain: licenses, bans, sources, advisories
deny:
    cargo deny check

# RustSec advisories against Cargo.lock
audit:
    cargo audit

# Unused dependencies
machete:
    cargo machete

# Build with each feature combination
hack:
    cargo hack check --feature-powerset --locked

# Install the pinned code generator if missing or at another version
tools:
    @[ "$(openapi-to-rust --version 2>/dev/null)" = "openapi-to-rust {{o2r_version}}" ] \
      || cargo install --locked openapi-to-rust --version {{o2r_version}}

# Download the latest Exa OpenAPI document (no-op when already current)
spec-update:
    @tmp=$(mktemp) && curl -fsSL {{spec_url}} -o "$tmp" \
      && if cmp -s "$tmp" {{spec}}; then echo "spec up to date"; rm -f "$tmp"; \
         else mv "$tmp" {{spec}}; echo "spec updated: {{spec}}"; fi

# Fail when the vendored spec differs from what exa.ai serves right now
spec-check:
    @tmp=$(mktemp) && curl -fsSL {{spec_url}} -o "$tmp" \
      && { cmp -s "$tmp" {{spec}} && echo "spec up to date"; rc=$?; rm -f "$tmp"; \
           [ $rc -eq 0 ] || { echo "spec drift: run 'just spec-sync'" >&2; exit 1; }; }

# Pull the latest spec, regenerate types, and prove the crate still compiles
spec-sync: spec-update codegen
    cargo check --all-targets --all-features --locked
    @git diff --stat -- {{spec}} src/generated 2>/dev/null || true

# Regenerate src/generated from the vendored spec
codegen: tools
    openapi-to-rust generate {{spec}} {{gen_args}}

# Fail if src/generated is stale relative to the spec
codegen-check: tools
    openapi-to-rust generate {{spec}} {{gen_args}} --check

# Everything the pre-push hook runs
ci: fmt-check codegen-check lint test doc deny audit machete

build:
    cargo build --release --locked

install:
    cargo install --path . --locked
