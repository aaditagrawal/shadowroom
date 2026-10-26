//! SSH-spawned, read-only remote photo access. See `docs/NETWORK.md` for the wire
//! format, threat model, lifecycle, and desktop integration contract.
mod client;
pub mod protocol;
mod server;

pub use client::{
    ConnectionSpec, PendingRequest, PreviewCache, PreviewResult, RemoteHost, TransferStats,
};
pub use protocol::{
    Hello, IndexSummary, PhotoKey, PhotoRecord, PhotoSync, Progress, RemoteRoot, Request, Response,
};
pub use server::{Agent, AgentOptions};

pub(crate) mod cache;
