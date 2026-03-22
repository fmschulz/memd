use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;

const ORT_DYLIB_ENV: &str = "ORT_DYLIB_PATH";
const ORT_DYLIB_OVERRIDE_ENV: &str = "MEMD_CROSS_ENCODER_ORT_DYLIB_PATH";
const ORT_VERSION_ENV: &str = "MEMD_CROSS_ENCODER_ORT_VERSION";
const ORT_URL_ENV: &str = "MEMD_CROSS_ENCODER_ORT_URL";
const ORT_DEFAULT_VERSION: &str = "1.23.2";
const ORT_DYLIB_MIN_BYTES: u64 = 1_000_000;
const ORT_DYLIB_NAME: &str = "libonnxruntime.so";

pub(crate) fn ensure_dylib(cache_dir: &Path) -> Result<PathBuf, String> {
    if let Some(path) = resolve_existing_from_env(ORT_DYLIB_ENV)? {
        ensure_library_path_hint(path.parent());
        return Ok(path);
    }

    if let Some(path) = resolve_existing_from_env(ORT_DYLIB_OVERRIDE_ENV)? {
        std::env::set_var(ORT_DYLIB_ENV, &path);
        ensure_library_path_hint(path.parent());
        return Ok(path);
    }

    let runtime_dir = cache_dir.join("onnxruntime");
    fs::create_dir_all(&runtime_dir).map_err(|err| {
        format!(
            "create ONNX runtime cache '{}': {err}",
            runtime_dir.display()
        )
    })?;

    if let Some(path) = find_dylib(&runtime_dir)? {
        std::env::set_var(ORT_DYLIB_ENV, &path);
        ensure_library_path_hint(path.parent());
        return Ok(path);
    }

    download_and_extract(&runtime_dir)?;
    let path = find_dylib(&runtime_dir)?.ok_or_else(|| {
        format!(
            "ONNX runtime download completed but no '{}' was found under '{}'",
            ORT_DYLIB_NAME,
            runtime_dir.display()
        )
    })?;
    std::env::set_var(ORT_DYLIB_ENV, &path);
    ensure_library_path_hint(path.parent());
    Ok(path)
}

fn resolve_existing_from_env(env_name: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw_path) = std::env::var(env_name).ok() else {
        return Ok(None);
    };
    let path = PathBuf::from(raw_path);
    verify_dylib(&path)?;
    Ok(Some(path))
}

fn verify_dylib(path: &Path) -> Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|err| format!("stat dylib '{}': {err}", path.display()))?;
    if metadata.len() < ORT_DYLIB_MIN_BYTES {
        return Err(format!(
            "dylib '{}' too small ({} bytes, expected >= {})",
            path.display(),
            metadata.len(),
            ORT_DYLIB_MIN_BYTES
        ));
    }
    Ok(())
}

fn find_dylib(root: &Path) -> Result<Option<PathBuf>, String> {
    for dir in candidate_dirs(root)? {
        let exact = dir.join(ORT_DYLIB_NAME);
        if verify_dylib(&exact).is_ok() {
            return Ok(Some(exact));
        }
    }

    let mut versioned = Vec::new();
    for dir in candidate_dirs(root)? {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("libonnxruntime.so.") || verify_dylib(&path).is_err() {
                continue;
            }
            let version = parse_version_suffix(name);
            versioned.push((version, path));
        }
    }

    versioned.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(versioned.pop().map(|(_, path)| path))
}

fn candidate_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs = vec![root.to_path_buf(), root.join("lib")];
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries {
            let entry = entry.map_err(|err| format!("read '{}': {err}", root.display()))?;
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            dirs.push(path.clone());
            dirs.push(path.join("lib"));
        }
    }
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

fn parse_version_suffix(name: &str) -> Vec<u32> {
    name.trim_start_matches("libonnxruntime.so.")
        .split('.')
        .filter_map(|token| token.parse::<u32>().ok())
        .collect()
}

fn download_and_extract(runtime_dir: &Path) -> Result<(), String> {
    let version =
        std::env::var(ORT_VERSION_ENV).unwrap_or_else(|_| ORT_DEFAULT_VERSION.to_string());
    let url = match std::env::var(ORT_URL_ENV) {
        Ok(url) => url,
        Err(_) => ort_release_url(&version)?,
    };
    let archive_path = runtime_dir.join(format!("onnxruntime-{version}.tgz"));
    if !archive_path.exists() {
        tracing::info!(url, destination = %archive_path.display(), "downloading ONNX runtime dylib archive");
        let mut response = ureq::get(&url)
            .call()
            .map_err(|err| format!("download ONNX runtime from '{}': {err}", url))?
            .into_reader();
        let mut file = fs::File::create(&archive_path)
            .map_err(|err| format!("create '{}': {err}", archive_path.display()))?;
        io::copy(&mut response, &mut file)
            .map_err(|err| format!("write '{}': {err}", archive_path.display()))?;
    }

    tracing::info!(archive = %archive_path.display(), destination = %runtime_dir.display(), "extracting ONNX runtime dylib archive");
    let archive_file = fs::File::open(&archive_path)
        .map_err(|err| format!("open '{}': {err}", archive_path.display()))?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);
    archive.unpack(runtime_dir).map_err(|err| {
        format!(
            "extract '{}' into '{}': {err}",
            archive_path.display(),
            runtime_dir.display()
        )
    })
}

fn ort_release_url(version: &str) -> Result<String, String> {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    match (os, arch) {
        ("linux", "x86_64") => Ok(format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{version}/onnxruntime-linux-x64-{version}.tgz"
        )),
        ("linux", "aarch64") => Ok(format!(
            "https://github.com/microsoft/onnxruntime/releases/download/v{version}/onnxruntime-linux-aarch64-{version}.tgz"
        )),
        _ => Err(format!(
            "automatic ONNX runtime download is unsupported on target '{os}/{arch}'; set '{}' to a valid libonnxruntime shared library path",
            ORT_DYLIB_OVERRIDE_ENV
        )),
    }
}

fn ensure_library_path_hint(parent: Option<&Path>) {
    let Some(parent) = parent.and_then(|path| path.to_str()) else {
        return;
    };
    let mut current = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    if current.split(':').any(|entry| entry == parent) {
        return;
    }
    if !current.is_empty() {
        current.push(':');
    }
    current.push_str(parent);
    std::env::set_var("LD_LIBRARY_PATH", current);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_suffix_extracts_numeric_components() {
        assert_eq!(
            parse_version_suffix("libonnxruntime.so.1.23.2"),
            vec![1, 23, 2]
        );
        assert_eq!(parse_version_suffix("libonnxruntime.so.bad.7"), vec![7]);
    }

    #[test]
    fn ort_release_url_supports_linux_targets() {
        let url = ort_release_url("1.23.2");
        if std::env::consts::OS == "linux" {
            assert!(url.as_deref().unwrap_or("").contains("onnxruntime-linux-"));
        } else {
            assert!(url.is_err());
        }
    }
}
