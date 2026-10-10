//! Extractions (ADR-DOC-02 §3, §3.2): the values an extraction holds, as
//! the contract has them, and the rules the OS checks on them.

mod value;

pub use value::{
    Anchor, Candidate, Engine, ExtractedValue, Geometry, MissingReason, Status, TextRange, has_null,
};
