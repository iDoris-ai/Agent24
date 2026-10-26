//! The client side of the Documenting OS routes (`/api/v1/documents`), shared
//! by the `agent24` CLI and the desktop, which runs `agent24 documents import`
//! instead of keeping its own copy (AGENTS.md: the CLI and the desktop share
//! their business logic). How the requests travel is the caller's
//! [`Transport`]; what to send, when to send it again and what the replies
//! mean is here, once.

pub mod import;
pub mod response;
pub mod transport;

pub use import::{CHUNK_BYTES, Progress, import_file};
pub use response::{ApiError, Response, to_response};
pub use transport::{Method, Reply, Request, Transport};
