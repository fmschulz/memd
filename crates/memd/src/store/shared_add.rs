//! Shared add-time chunk splitting logic for storage backends.
//!
//! Keeps `MemoryStore` and `PersistentStore` behavior consistent for long documents.

use crate::chunking::{chunk_text, ChunkingConfig};
use crate::types::MemoryChunk;

/// Split threshold for long-document chunking.
pub const ADD_CHUNK_THRESHOLD: usize = 1000;

/// Split a chunk into sub-chunks when document text is long.
///
/// Returns at least one chunk.
pub fn split_for_add(chunk: MemoryChunk) -> Vec<MemoryChunk> {
    if chunk.text.len() <= ADD_CHUNK_THRESHOLD {
        return vec![chunk];
    }

    let text_chunks = chunk_text(&chunk.text, &ChunkingConfig::default());
    if text_chunks.len() <= 1 {
        return vec![chunk];
    }

    let total_chunks = text_chunks.len();
    text_chunks
        .into_iter()
        .enumerate()
        .map(|(idx, text_chunk)| {
            let mut sub_chunk = chunk.clone();
            sub_chunk.text = text_chunk.text;
            sub_chunk.tags.push(format!("chunk_index:{}", idx));
            sub_chunk
                .tags
                .push(format!("total_chunks:{}", total_chunks));
            sub_chunk
                .tags
                .push(format!("char_start:{}", text_chunk.start_char));
            sub_chunk
                .tags
                .push(format!("char_end:{}", text_chunk.end_char));
            sub_chunk
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChunkType, TenantId};
    use proptest::prelude::*;

    fn make_chunk(text: &str) -> MemoryChunk {
        let tenant = TenantId::new("test_tenant").expect("valid tenant");
        MemoryChunk::new(tenant, text, ChunkType::Doc)
    }

    proptest! {
        #[test]
        fn short_documents_are_not_split(text in "[A-Za-z0-9 ]{0,1000}") {
            let chunk = make_chunk(&text);
            let chunks = split_for_add(chunk.clone());

            prop_assert_eq!(chunks.len(), 1);
            prop_assert_eq!(chunks[0].text.as_str(), chunk.text.as_str());
            prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("chunk_index:")));
            prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("total_chunks:")));
            prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("char_start:")));
            prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("char_end:")));
        }
    }

    proptest! {
        #[test]
        fn long_documents_emit_consistent_chunk_metadata(repeats in 20usize..80usize) {
            let text = "This is a long sentence that should trigger chunk splitting behavior. ".repeat(repeats);
            let chunks = split_for_add(make_chunk(&text));

            if chunks.len() == 1 {
                prop_assert_eq!(chunks[0].text.as_str(), text.as_str());
                prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("chunk_index:")));
                prop_assert!(chunks[0].tags.iter().all(|t| !t.starts_with("total_chunks:")));
            } else {
                let total_tag = format!("total_chunks:{}", chunks.len());
                for (idx, chunk) in chunks.iter().enumerate() {
                    prop_assert!(!chunk.text.is_empty());
                    prop_assert_eq!(chunk.tenant_id.as_str(), "test_tenant");
                    prop_assert_eq!(chunk.chunk_type, ChunkType::Doc);
                    let has_index_tag = chunk.tags.contains(&format!("chunk_index:{}", idx));
                    prop_assert!(has_index_tag);
                    prop_assert!(chunk.tags.contains(&total_tag));
                    prop_assert!(chunk.tags.iter().any(|t| t.starts_with("char_start:")));
                    prop_assert!(chunk.tags.iter().any(|t| t.starts_with("char_end:")));
                }
            }
        }
    }
}
