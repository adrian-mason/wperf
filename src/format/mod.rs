//! wPRF v1 binary format — header, event types, TLV records, and writer.
//!
//! Layout: `[64B Header] [TLV Record]* [Footer Section Table]`
//!
//! Reference: final-design.md §4, ADR-010.

pub mod event;
pub mod header;
pub mod writer;
