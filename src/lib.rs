//! Exa API client backed by a persistent, round-robin pool of API keys.
//!
//! The crate is split so that the rotation policy can be tested without a
//! network:
//!
//! - [`policy`] turns an HTTP response into a [`policy::Verdict`] (pure).
//! - [`state`] persists per-key health and the round-robin cursor on disk.
//! - [`pool`] selects keys, runs requests through a [`transport::Transport`],
//!   and applies verdicts to the state.
//! - [`exa`] builds request bodies for the Exa endpoints.
//! - [`cli`] is the clap surface consumed by the binary.

pub mod cli;
pub mod config;
pub mod error;
pub mod exa;
/// Types generated from `spec/exa-spec.json` by `openapi-to-rust`; see `just codegen`.
#[allow(
    missing_docs,
    unreachable_pub,
    unused_qualifications,
    missing_debug_implementations,
    trivial_casts,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    rust_2018_idioms,
    rustdoc::all,
    reason = "generated code is checked by the generator's own tests, not by our lints"
)]
#[rustfmt::skip]
pub mod generated;
pub mod policy;
pub mod pool;
pub mod state;
pub mod transport;
