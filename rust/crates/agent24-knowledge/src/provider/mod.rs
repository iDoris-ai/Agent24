//! The host-side KnowledgeProvider (KB-C1-04, CONTRACT-STATE-v1), built up in
//! slices: the state judgements (`state`) and one binding's generation and
//! calls in flight (`gate`); events and persistence follow.

pub mod gate;
pub mod state;
