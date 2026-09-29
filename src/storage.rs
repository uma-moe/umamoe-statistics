use anyhow::{anyhow, Context, Result};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::statistics::{DATA_FORMAT, DATA_FORMAT_VERSION};
pub(crate) fn updateMasterIndex(
    output_root: &Path,
    dataset_version: &str,
    dataset_name: &str,
    generated_at: &str,
    index: Value,
) -> Result<()> {
    let path = output_root.join("datasets.json");
    let mut master = if jsonStorageExists(&path) {
        readJson(&path)?
    } else {
        json!({"datasets": [], "last_updated": generated_at})
    };

    let datasets = master
        .get_mut("datasets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("{} does not contain a datasets array", path.display()))?;
    datasets.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(dataset_version));
    datasets.push(json!({
        "id": dataset_version,
        "version": dataset_version,
        "name": dataset_name,
        "format": DATA_FORMAT,
        "format_version": DATA_FORMAT_VERSION,
        "date": generated_at,
        "basePath": format!("/assets/statistics/{dataset_version}"),
        "index": index
    }));
    datasets.sort_by(|left, right| {
        let left_date = left.get("date").and_then(Value::as_str).unwrap_or_default();
        let right_date = right
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or_default();
        right_date.cmp(left_date)
    });
    master["last_updated"] = json!(generated_at);
    writeJsonPlain(&path, &master)
}

pub(crate) fn writeJsonPlain(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(value)?;
    fs::write(path, &bytes).with_context(|| format!("write {}", path.display()))?;

    let gz_path = gzipPath(path);
    if gz_path.exists() {
        fs::remove_file(&gz_path).with_context(|| format!("remove {}", gz_path.display()))?;
    }

    Ok(())
}

pub(crate) fn writeJson(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(value)?;
    let gz_path = gzipPath(path);
    let gz_bytes = gzipBytes(&bytes)?;
    fs::write(&gz_path, gz_bytes).with_context(|| format!("write {}", gz_path.display()))?;

    if path.exists() {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }

    Ok(())
}

fn gzipPath(path: &Path) -> PathBuf {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => path.with_extension(format!("{extension}.gz")),
        None => path.with_extension("gz"),
    }
}

fn gzipBytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes)?;
    encoder.finish().map_err(Into::into)
}

fn jsonStorageExists(path: &Path) -> bool {
    path.exists() || gzipPath(path).exists()
}

pub(crate) fn replaceDirectory(
    staging_root: &Path,
    dataset_root: &Path,
    backup_root: &Path,
) -> Result<()> {
    removePath(backup_root)?;

    let had_existing = dataset_root.exists();
    if had_existing {
        fs::rename(dataset_root, backup_root).with_context(|| {
            format!(
                "move existing dataset {} to {}",
                dataset_root.display(),
                backup_root.display()
            )
        })?;
    }

    if let Err(error) = fs::rename(staging_root, dataset_root) {
        if had_existing {
            let _ = fs::rename(backup_root, dataset_root);
        }
        return Err(error).with_context(|| {
            format!(
                "publish staged dataset {} to {}",
                staging_root.display(),
                dataset_root.display()
            )
        });
    }

    if had_existing {
        removePath(backup_root)?;
    }

    Ok(())
}

pub(crate) fn removePath(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

fn readJson(path: &Path) -> Result<Value> {
    if path.exists() {
        let content =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        return serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()));
    }

    let gz_path = gzipPath(path);
    let compressed = fs::read(&gz_path).with_context(|| format!("read {}", gz_path.display()))?;
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut content = String::new();
    decoder
        .read_to_string(&mut content)
        .with_context(|| format!("decompress {}", gz_path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse {}", gz_path.display()))
}
