//! Local operation handlers and compatibility internals.
//!
//! The agent-facing integration path is the `memd` CLI. This module keeps
//! the shared operation handlers used by `memd call`, `memd batch`, and the
//! direct CLI commands.

pub mod dedup;
pub mod digest_sweeper;
pub mod error;
pub mod handlers;
pub mod markdown_export;
pub mod post_write_hooks;

pub use digest_sweeper::{spawn_digest_sweeper, DigestSweeperHandle};
pub use error::McpError;
pub use handlers::*;
pub use post_write_hooks::PostWriteEvent;
