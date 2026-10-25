//! The host-side KnowledgeProvider (KB-C1-04, CONTRACT-STATE-v1), built up in
//! slices: the state judgements (`state`), one binding's generation and calls
//! in flight (`gate`), its connection phase and probe rounds (`connection`),
//! its credentials (`credential`) and the binding as a whole with its user and
//! configuration events (`binding`), the unbound placeholder (`placeholder`)
//! and the profile with its identity changes (`profile`); persistence follows.

pub mod binding;
pub mod connection;
pub mod credential;
pub mod gate;
pub mod placeholder;
pub mod profile;
pub mod state;
