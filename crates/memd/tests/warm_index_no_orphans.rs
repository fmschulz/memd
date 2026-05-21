//! Migration test: loading an index that has orphan graph-NNNN snapshots
//! should silently delete them so freshly-upgraded installs reclaim disk
//! without waiting for the next save.

use memd::index::{HnswConfig, HnswIndex};
use memd::types::ChunkId;
use std::fs::{self, File};
use std::io::Write;

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[test]
fn loading_index_purges_legacy_orphan_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm_index");
    fs::create_dir_all(&path).unwrap();

    // Seed a real, working index first.
    let config = HnswConfig {
        max_elements: 100,
        dimension: 4,
        ..Default::default()
    };
    {
        let index = HnswIndex::with_persistence(config.clone(), &path).unwrap();
        let mut emb = vec![1.0, 0.0, 0.0, 0.0];
        normalize(&mut emb);
        index.insert(&ChunkId::new(), &emb).unwrap();
        index.save().unwrap();
    }

    // Simulate legacy bloat: write garbage orphan snapshots.
    for n in [1645u32, 6302, 9776] {
        for ext in ["hnsw.graph", "hnsw.data"] {
            let p = path.join(format!("graph-{n}.{ext}"));
            let mut f = File::create(&p).unwrap();
            f.write_all(&vec![0u8; 1024]).unwrap();
        }
    }

    // Reloading should sweep them away.
    let _index = HnswIndex::with_persistence(config, &path).unwrap();

    for entry in fs::read_dir(&path).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            !(name.starts_with("graph-") && name.contains(".hnsw.")),
            "orphan snapshot still present: {name}"
        );
    }
}
