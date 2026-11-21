//! Writer-only rebuild marker for the BM25 analyzer.
//!
//! Revision 2: every character that is not a letter or digit separates tokens.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::Result;

pub(super) const REVISION: &str = "2";
const MARKER: &str = "sparse_revision";

fn write_marker(data_dir: &Path, value: &str) -> Result<()> {
    let temporary = data_dir.join("sparse_revision.tmp");
    let mut file = fs::File::create(&temporary)?;
    writeln!(file, "{value}")?;
    file.sync_all()?;
    fs::rename(temporary, data_dir.join(MARKER))?;
    fs::File::open(data_dir)?.sync_all()?;
    Ok(())
}

/// Discard a stale index before opening its writer. An interrupted rebuild
/// starts over, so a partially committed index cannot be mistaken for current.
pub(super) fn prepare(data_dir: &Path) -> Result<bool> {
    if fs::read_to_string(data_dir.join(MARKER)).is_ok_and(|value| value.trim() == REVISION)
        && data_dir.join("sparse_index").is_dir()
    {
        return Ok(false);
    }
    let index = data_dir.join("sparse_index");
    if index.is_dir() {
        fs::remove_dir_all(index)?;
    }
    write_marker(data_dir, &format!("{REVISION} rebuilding"))?;
    Ok(true)
}

/// Call only after all payloads were indexed and the Tantivy commit succeeded.
pub(super) fn mark_current(data_dir: &Path) -> Result<()> {
    write_marker(data_dir, REVISION)
}
