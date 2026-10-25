//! The host-side KnowledgeProvider (KB-C1-04, CONTRACT-STATE-v1), built up in
//! slices: the state judgements (`state`), one binding's generation and calls
//! in flight (`gate`), its connection phase and probe rounds (`connection`)
//! and its credentials (`credential`); events and persistence follow.

pub mod connection;
pub mod credential;
pub mod gate;
pub mod state;
