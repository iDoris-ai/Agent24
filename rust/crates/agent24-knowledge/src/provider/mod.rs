//! The host-side KnowledgeProvider (KB-C1-04, CONTRACT-STATE-v1), built up in
//! slices: the state judgements (`state`), one binding's generation and calls
//! in flight (`gate`), its connection phase and probe rounds (`connection`),
//! its credentials (`credential`) and the binding as a whole with its user and
//! configuration events (`binding`), the unbound placeholder (`placeholder`),
//! the profile with its identity changes (`profile`), what the history shows
//! of the evidence a binding delivered (`evidence`), and what is persisted
//! when, with the seal that tells a restart whether it can trust that
//! (`ledger`), the records it is persisted as (`record`), how they are read
//! back (`replay`) and what a restart makes of them (`recover`), with the
//! order its writes run in (`journal`); the budgets calls run under
//! (`budget`); one profile as the host holds it (`member`), and the
//! orchestration over them all (`host`).

pub mod binding;
pub mod budget;
pub mod connection;
pub mod credential;
pub mod evidence;
pub mod gate;
pub mod host;
pub mod journal;
pub mod ledger;
pub mod member;
pub mod placeholder;
pub mod profile;
pub mod record;
pub mod recover;
pub mod replay;
pub mod state;
pub mod stored;
