//! Shared read helpers used by CLI dispatch.

use crate::error::Result;
use crate::store::Store;
use crate::types::{MemoryChunk, TenantId};

pub(super) async fn collect_all_chunks<S: Store>(
    store: &S,
    tenant: &TenantId,
    page_size: usize,
) -> Result<Vec<MemoryChunk>> {
    let mut offset = 0usize;
    let mut chunks = Vec::new();

    loop {
        let page = store.list_chunks(tenant, page_size, offset).await?;
        if page.is_empty() {
            break;
        }
        chunks.extend(page);
        offset += page_size;
    }

    Ok(chunks)
}
