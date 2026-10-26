//! The client side of the Documenting OS routes (`/api/v1/documents`), shared
//! by the `agent24` CLI and the desktop, which runs `agent24 documents import`
//! instead of keeping its own copy (AGENTS.md: the CLI and the desktop share
//! their business logic). How the requests travel is the caller's
//! [`Transport`]; what to send, when to send it again and what the replies
//! mean is here, once. The desktop's other document operations
//! (`window.agent24.documents.request`) are here too ([`ops`]).

pub mod http;
pub mod import;
pub mod ops;
pub mod response;
pub mod transport;

pub use import::{CHUNK_BYTES, Progress, import_file};
pub use ops::{call, route};
pub use response::{ApiError, Response, to_response};
pub use transport::{Method, Reply, Request, Transport};
