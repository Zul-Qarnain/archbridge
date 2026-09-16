use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileReview {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub executable: bool,
    pub content: Option<String>,
}

pub struct SourceSnapshot {
    pub tarball_bytes: Vec<u8>,
    pub sha256_hash: String,
    pub reviews: Vec<FileReview>,
}

pub fn snapshot_directory(dir_path: &Path) -> Result<SourceSnapshot, String> {
    if !dir_path.exists() || !dir_path.is_dir() {
        return Err(format!(
            "Directory does not exist or is not a directory: '{:?}'",
            dir_path
        ));
    }

    let mut tar_builder = tar::Builder::new(Vec::new());
    let mut reviews = Vec::new();

    let mut stack = vec![dir_path.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read directory '{:?}': {}", current_dir, e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            let rel_path = path.strip_prefix(dir_path).map_err(|e| e.to_string())?;
            let rel_str = rel_path.to_string_lossy().to_string();

            if rel_str.starts_with(".git")
                || rel_str.starts_with(".archbridge")
                || rel_str.contains("target/")
            {
                continue;
            }

            // Path safety check
            if rel_str.contains("..") || rel_path.is_absolute() {
                return Err(format!(
                    "Unsafe path detected in source directory: '{}'",
                    rel_str
                ));
            }

            let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(|e| e.to_string())?;
                if target.is_absolute() || target.to_string_lossy().contains("..") {
                    return Err(format!(
                        "Unsafe symlink in source directory: '{}' -> '{:?}'",
                        rel_str, target
                    ));
                }
            } else if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let bytes = fs::read(&path).map_err(|e| e.to_string())?;
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let sha256 = format!("{:x}", hasher.finalize());

                #[cfg(unix)]
                let is_exec = {
                    use std::os::unix::fs::PermissionsExt;
                    (meta.permissions().mode() & 0o111) != 0
                };
                #[cfg(not(unix))]
                let is_exec = false;

                let content_preview = if bytes.len() < 10_000 && !bytes.contains(&0) {
                    String::from_utf8(bytes.clone()).ok()
                } else {
                    None
                };

                reviews.push(FileReview {
                    path: rel_str.clone(),
                    sha256,
                    bytes: bytes.len() as u64,
                    executable: is_exec,
                    content: content_preview,
                });

                tar_builder
                    .append_path_with_name(&path, &rel_str)
                    .map_err(|e| format!("Failed to archive file '{}': {}", rel_str, e))?;
            }
        }
    }

    let raw_tar = tar_builder
        .into_inner()
        .map_err(|e| format!("Failed to finish tar archive: {}", e))?;
    let mut gz_encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    use std::io::Write;
    gz_encoder
        .write_all(&raw_tar)
        .map_err(|e| format!("Failed to compress tar archive: {}", e))?;
    let tarball_bytes = gz_encoder
        .finish()
        .map_err(|e| format!("Failed to finalize gzip tarball: {}", e))?;

    let mut hasher = Sha256::new();
    hasher.update(&tarball_bytes);
    let sha256_hash = format!("{:x}", hasher.finalize());

    Ok(SourceSnapshot {
        tarball_bytes,
        sha256_hash,
        reviews,
    })
}

pub fn snapshot_tarball_bytes(bytes: &[u8]) -> Result<SourceSnapshot, String> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sha256_hash = format!("{:x}", hasher.finalize());

    let gz_decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz_decoder);

    let mut reviews = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to parse tar archive entries: {}", e))?;

    for entry_res in entries {
        let mut entry = entry_res.map_err(|e| format!("Failed to read archive entry: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("Invalid path in archive entry: {}", e))?;
        let path_str = path.to_string_lossy().to_string();

        if path_str.contains("..") || path.is_absolute() {
            return Err(format!("Unsafe path in source archive: '{}'", path_str));
        }

        let mut file_bytes = Vec::new();
        entry
            .read_to_end(&mut file_bytes)
            .map_err(|e| format!("Failed to read archive file '{}': {}", path_str, e))?;

        let mut file_hasher = Sha256::new();
        file_hasher.update(&file_bytes);
        let file_sha = format!("{:x}", file_hasher.finalize());

        let mode = entry.header().mode().unwrap_or(0);
        let is_exec = (mode & 0o111) != 0;

        let content_preview = if file_bytes.len() < 10_000 && !file_bytes.contains(&0) {
            String::from_utf8(file_bytes.clone()).ok()
        } else {
            None
        };

        reviews.push(FileReview {
            path: path_str,
            sha256: file_sha,
            bytes: file_bytes.len() as u64,
            executable: is_exec,
            content: content_preview,
        });
    }

    Ok(SourceSnapshot {
        tarball_bytes: bytes.to_vec(),
        sha256_hash,
        reviews,
    })
}
