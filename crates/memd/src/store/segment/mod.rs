//! Segment storage module
//!
//! Segments are append-only files containing chunk payloads.
//! Directory structure: tenants/<tenant_id>/segments/seg_<id>/

pub mod format;
pub mod reader;
pub mod writer;

pub use format::{PayloadIndexRecord, SEGMENT_MAGIC, SegmentMeta};
pub use reader::SegmentReader;
pub use writer::SegmentWriter;
