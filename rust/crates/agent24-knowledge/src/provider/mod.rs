//! The host-side KnowledgeProvider (KB-C1-04, CONTRACT-STATE-v1), built up in
//! slices: the state judgements (`state`), one binding's generation and calls
//! in flight (`gate`) and its connection phase and probe rounds
//! (`connection`); events and persistence follow.

pub mod connection;
pub mod gate;
pub mod state;
