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

/// Bytes of a file kept verbatim in a review so a human can read it.
const PREVIEW_LIMIT: usize = 10_000;

/// Upper bound on what a source snapshot may hold once unpacked. Snapshots
/// live in memory until the build runs; a gzip bomb or a forgotten multi-GB
/// asset directory must fail with a message instead of exhausting RAM.
pub const MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Hash a stream without holding it: returns the digest, the byte count and
/// the first bytes for a human-readable preview.
pub fn digest_reader<R: Read>(mut reader: R) -> std::io::Result<(String, u64, Vec<u8>)> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut preview = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let bytes: &[u8] = &buf[..n];
        hasher.update(bytes);
        if preview.len() <= PREVIEW_LIMIT {
            let take = (PREVIEW_LIMIT + 1 - preview.len()).min(n);
            preview.extend_from_slice(&bytes[..take]);
        }
        total += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total, preview))
}

/// SHA-256 of a file, streamed.
pub fn sha256_path(path: &Path) -> Result<String, String> {
    let file =
        fs::File::open(path).map_err(|e| format!("Cannot read '{}': {}", path.display(), e))?;
    digest_reader(file)
        .map(|(digest, _, _)| digest)
        .map_err(|e| format!("Cannot read '{}': {}", path.display(), e))
}

fn preview_text(preview: Vec<u8>, total: u64) -> Option<String> {
    if total as usize > PREVIEW_LIMIT || total as usize != preview.len() || preview.contains(&0) {
        None
    } else {
        String::from_utf8(preview).ok()
    }
}

/// Review every regular file under `root`, addressed by its path relative to
/// `root`. Each entry hashes the file's own bytes, so the manifest states what
/// a later step can verify rather than a value derived from the file name.
pub fn walk_tree(root: &Path) -> Result<Vec<FileReview>, String> {
    let mut reviews = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read directory '{:?}': {}", current_dir, e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let rel_str = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_string();
            let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;

            if meta.is_dir() {
                stack.push(path);
            } else if meta.file_type().is_symlink() {
                reviews.push(symlink_review(rel_str, &path));
            } else if meta.is_file() {
                let file = fs::File::open(&path)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;
                let (sha256, bytes, preview) = digest_reader(file)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;
                reviews.push(FileReview {
                    path: rel_str,
                    sha256,
                    bytes,
                    executable: is_executable(&meta),
                    content: preview_text(preview, bytes),
                });
            }
        }
    }
    reviews.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(reviews)
}

/// A link carries no bytes of its own, so the manifest records the target
/// instead of pretending to hash content.
fn symlink_review(rel_str: String, path: &Path) -> FileReview {
    let target = fs::read_link(path)
        .map(|t| t.to_string_lossy().to_string())
        .unwrap_or_default();
    FileReview {
        path: rel_str,
        sha256: format!("{:x}", Sha256::digest(target.as_bytes())),
        bytes: 0,
        executable: false,
        content: Some(format!("symlink -> {}", target)),
    }
}

fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// Top-level entries that are never part of a source snapshot: VCS metadata,
/// ArchBridge's own job workspace, and build output trees.
fn excluded_from_snapshot(rel: &Path) -> bool {
    let first = rel
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default();
    matches!(
        first.as_str(),
        ".git" | ".hg" | ".svn" | ".archbridge" | "target" | "graphify-out"
    )
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
    let mut total: u64 = 0;

    let mut stack = vec![dir_path.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read directory '{:?}': {}", current_dir, e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            let rel_path = path.strip_prefix(dir_path).map_err(|e| e.to_string())?;
            let rel_str = rel_path.to_string_lossy().to_string();

            // Matched per path component: a prefix test also dropped
            // `.github/` and `.gitmodules`, and a substring test dropped any
            // directory merely ending in "target".
            if excluded_from_snapshot(rel_path) {
                continue;
            }

            // Path safety check
            if rel_path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
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
                // In-tree links are part of the source (shared headers, version
                // aliases); dropping them silently broke builds that use them.
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                tar_builder
                    .append_link(&mut header, &rel_str, &target)
                    .map_err(|e| format!("Failed to archive link '{}': {}", rel_str, e))?;
                reviews.push(symlink_review(rel_str, &path));
            } else if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                total += meta.len();
                if total > MAX_SNAPSHOT_BYTES {
                    return Err(format!(
                        "Source directory '{}' holds more than {} GiB; point the build at the \
                         project itself, not a parent directory",
                        dir_path.display(),
                        MAX_SNAPSHOT_BYTES / 1024 / 1024 / 1024
                    ));
                }
                let file = fs::File::open(&path)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;
                let (sha256, bytes, preview) = digest_reader(file)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;

                reviews.push(FileReview {
                    path: rel_str.clone(),
                    sha256,
                    bytes,
                    executable: is_executable(&meta),
                    content: preview_text(preview, bytes),
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

    let sha256_hash = format!("{:x}", Sha256::digest(&tarball_bytes));
    reviews.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(SourceSnapshot {
        tarball_bytes,
        sha256_hash,
        reviews,
    })
}

pub fn snapshot_tarball_bytes(bytes: &[u8]) -> Result<SourceSnapshot, String> {
    let sha256_hash = format!("{:x}", Sha256::digest(bytes));

    let gz_decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz_decoder);

    let mut reviews = Vec::new();
    let mut total: u64 = 0;
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to parse tar archive entries: {}", e))?;

    for entry_res in entries {
        let entry = entry_res.map_err(|e| format!("Failed to read archive entry: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("Invalid path in archive entry: {}", e))?
            .into_owned();
        let path_str = path.to_string_lossy().to_string();

        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
            || path.is_absolute()
        {
            return Err(format!("Unsafe path in source archive: '{}'", path_str));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let mode = entry.header().mode().unwrap_or(0);
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .ok()
                .flatten()
                .map(|t| t.to_string_lossy().to_string())
                .unwrap_or_default();
            reviews.push(FileReview {
                path: path_str,
                sha256: format!("{:x}", Sha256::digest(target.as_bytes())),
                bytes: 0,
                executable: false,
                content: Some(format!("symlink -> {}", target)),
            });
            continue;
        }
        if !kind.is_file() {
            continue;
        }

        // Streamed with a running cap: the compressed size says nothing about
        // what an archive expands to.
        let remaining = MAX_SNAPSHOT_BYTES.saturating_sub(total);
        let (file_sha, size, preview) = digest_reader(entry.take(remaining + 1))
            .map_err(|e| format!("Failed to read archive file '{}': {}", path_str, e))?;
        total += size;
        if total > MAX_SNAPSHOT_BYTES {
            return Err(format!(
                "Source archive expands beyond {} GiB; refusing to review it",
                MAX_SNAPSHOT_BYTES / 1024 / 1024 / 1024
            ));
        }

        reviews.push(FileReview {
            path: path_str,
            sha256: file_sha,
            bytes: size,
            executable: (mode & 0o111) != 0,
            content: preview_text(preview, size),
        });
    }

    Ok(SourceSnapshot {
        tarball_bytes: bytes.to_vec(),
        sha256_hash,
        reviews,
    })
}

/// Read one regular file out of a gzip tarball held in memory.
pub fn read_tarball_entry(bytes: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to parse tar archive entries: {}", e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("Failed to read archive entry: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("Invalid path in archive entry: {}", e))?
            .to_string_lossy()
            .to_string();
        if path.trim_start_matches("./") == wanted && entry.header().entry_type().is_file() {
            let mut out = Vec::new();
            entry
                .read_to_end(&mut out)
                .map_err(|e| format!("Failed to read '{}': {}", wanted, e))?;
            return Ok(out);
        }
    }
    Err(format!("'{}' is not in the archive", wanted))
}

/// Unpack the pinned gzip tarball into `dest`, optionally dropping a wrapping
/// directory. Only regular files, directories and relative in-tree symlinks are
/// written; absolute paths, `..` components and device nodes are refused, so a
/// hostile archive cannot write outside the job directory.
pub fn unpack_tarball_tree(
    bytes: &[u8],
    dest: &Path,
    strip_prefix: Option<&str>,
) -> Result<(), String> {
    use std::path::Component;

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to parse tar archive entries: {}", e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("Failed to read archive entry: {}", e))?;
        let raw = entry
            .path()
            .map_err(|e| format!("Invalid path in archive entry: {}", e))?
            .into_owned();
        let mut rel = raw.as_path();
        if let Ok(stripped) = rel.strip_prefix(".") {
            rel = stripped;
        }
        if let Some(prefix) = strip_prefix {
            match rel.strip_prefix(prefix) {
                Ok(stripped) => rel = stripped,
                Err(_) => continue,
            }
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!(
                "Unsafe path in source archive: '{}'",
                raw.display()
            ));
        }
        let target = dest.join(rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|e| format!("Failed to create '{}': {}", rel.display(), e))?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
            }
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|e| format!("Failed to read '{}': {}", rel.display(), e))?;
            fs::write(&target, &data)
                .map_err(|e| format!("Failed to write '{}': {}", rel.display(), e))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.header().mode().unwrap_or(0o644) & 0o755;
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode | 0o600));
            }
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(|e| format!("Invalid link in '{}': {}", rel.display(), e))?
                .ok_or_else(|| format!("Symlink '{}' has no target", rel.display()))?;
            let link_text = link.to_string_lossy();
            if link.is_absolute() || link_text.contains("..") {
                return Err(format!(
                    "Unsafe symlink in source archive: '{}' -> '{}'",
                    rel.display(),
                    link_text
                ));
            }
            #[cfg(unix)]
            {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::os::unix::fs::symlink(&*link, &target)
                    .map_err(|e| format!("Failed to link '{}': {}", rel.display(), e))?;
            }
        }
        // Device nodes, FIFOs and hard links carry nothing a build needs.
    }
    Ok(())
}
