//! Segment storage module
//!
//! Segments are append-only files containing chunk payloads.
//! Directory structure: tenants/<tenant_id>/segments/seg_<id>/

pub mod format;
pub mod reader;
pub mod writer;

pub use format::{PayloadIndexRecord, SegmentMeta, SEGMENT_MAGIC};
pub use reader::SegmentReader;
pub use writer::SegmentWriter;
