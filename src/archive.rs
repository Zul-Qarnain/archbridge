use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

/// Largest file an archive or tree may contribute to a snapshot. The digest is
/// streamed, so this bounds the work and the tarball, not the resident memory.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Largest total of files a snapshot may hold.
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest number of entries reviewed in one snapshot. A tar bomb of a million
/// paths would fill the review a human is supposed to read.
const MAX_ENTRIES: usize = 20_000;
/// Bytes kept for the displayable text preview of a file.
const PREVIEW_BYTES: usize = 10_000;

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

/// Hash a stream without holding it in memory, keeping only the leading bytes
/// needed for a text preview. Exceeding `limit` is an error rather than a slow
/// burn, so a lying archive header or an unexpectedly huge tree stops here.
struct StreamDigest {
    sha256: String,
    bytes: u64,
    preview: Option<String>,
}

fn digest_stream<R: Read>(reader: &mut R, limit: u64) -> Result<StreamDigest, String> {
    let mut hasher = Sha256::new();
    let mut prefix: Vec<u8> = Vec::with_capacity(PREVIEW_BYTES + 1);
    let mut total: u64 = 0;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|e| format!("Failed to read the archived bytes: {e}"))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            return Err(format!(
                "entry exceeds the maximum snapshot file size of {} bytes",
                limit
            ));
        }
        hasher.update(&chunk[..read]);
        if prefix.len() <= PREVIEW_BYTES {
            let room = PREVIEW_BYTES + 1 - prefix.len();
            prefix.extend_from_slice(&chunk[..read.min(room)]);
        }
    }

    // The preview is only ever text: a NUL anywhere in what was kept, or more
    // bytes than the preview window, means a human reads a summary instead.
    let preview = if total <= PREVIEW_BYTES as u64 && !prefix.contains(&0) {
        String::from_utf8(prefix).ok()
    } else {
        None
    };
    Ok(StreamDigest {
        sha256: format!("{:x}", hasher.finalize()),
        bytes: total,
        preview,
    })
}

/// Decompress a `.tar.gz` frame into memory, refusing anything that grows past
/// the snapshot budget. A gzip bomb is otherwise indistinguishable from a
/// legitimate archive until the process is killed.
fn inflate_bounded(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut tar_bytes = Vec::new();
    decoder
        .read_to_end_with_limit(&mut tar_bytes, MAX_TOTAL_BYTES)
        .map_err(|e| format!("Failed to decompress the source archive: {e}"))?;
    Ok(tar_bytes)
}

/// A tar stream ends with two zero blocks. Without them the listing is a
/// truncated read, and a review that quietly omits files is worse than an error.
fn require_tar_terminator(tar_bytes: &[u8]) -> Result<(), String> {
    if tar_bytes.len() < 1024
        || !tar_bytes[tar_bytes.len() - 1024..]
            .iter()
            .all(|byte| *byte == 0)
    {
        return Err("The source archive is truncated: it has no end-of-archive marker".to_string());
    }
    Ok(())
}

trait ReadToEndWithLimit: Read {
    fn read_to_end_with_limit(&mut self, buffer: &mut Vec<u8>, limit: u64) -> std::io::Result<()>;
}

impl<R: Read> ReadToEndWithLimit for R {
    fn read_to_end_with_limit(&mut self, buffer: &mut Vec<u8>, limit: u64) -> std::io::Result<()> {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let read = self.read(&mut chunk)?;
            if read == 0 {
                return Ok(());
            }
            if buffer.len() as u64 + read as u64 > limit {
                return Err(std::io::Error::other(format!(
                    "the archive expands to more than {} bytes, over the snapshot limit",
                    limit
                )));
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// A path a snapshot may carry: relative, inside the tree, and not a name that
/// escapes it when a tool joins it onto a destination directory.
fn reject_unsafe_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Unsafe path in source archive: the entry names nothing".to_string());
    }
    if path.contains('\0') {
        return Err(format!(
            "Unsafe path in source archive: '{path}' contains a NUL byte"
        ));
    }
    if path.starts_with('/') {
        return Err(format!("Unsafe path in source archive: '{path}'"));
    }
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("Unsafe path in source archive: '{path}'"));
    }
    Ok(())
}

/// The bytes a link entry points at, or nothing when the archive left it out.
fn link_target<R: Read>(entry: &mut tar::Entry<R>) -> Result<String, std::io::Error> {
    Ok(match entry.link_name()? {
        Some(name) => name.to_string_lossy().to_string(),
        None => return Err(std::io::Error::other("the link entry names no target")),
    })
}

/// A link target must land inside the extracted tree, which is what makes it
/// safe for a build to unpack the archive it came from. `..` is only dangerous
/// when it walks out of the tree: `dir/x -> ../file` stays inside, while
/// `dir/x -> ../../etc/shadow` does not, so the target is resolved against the
/// entry's own directory rather than merely scanned for dot-dot.
fn reject_unsafe_link(kind: &str, path: &str, target: &str) -> Result<(), String> {
    let unsafe_link = |why: &str| {
        Err(format!(
            "Unsafe {kind} in source archive: '{path}' -> '{target}' {why}"
        ))
    };
    if target.is_empty() {
        return unsafe_link("names no target");
    }
    if target.contains('\0') {
        return unsafe_link("contains a NUL byte");
    }
    if target.starts_with('/') {
        return unsafe_link("is absolute");
    }
    // Depth of the entry's directory, then consume the target component by
    // component; running out of depth on a `..` is the escape.
    let mut depth = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .count()
        .saturating_sub(1);
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                depth = match depth.checked_sub(1) {
                    Some(next) => next,
                    None => return unsafe_link("escapes the extracted tree"),
                };
            }
            other => {
                let _ = other;
                depth += 1;
            }
        }
    }
    Ok(())
}

/// Review every regular file under `root`, addressed by its path relative to
/// `root`. Each entry hashes the file's own bytes, so the manifest states what
/// a later step can verify rather than a value derived from the file name.
pub fn walk_tree(root: &Path) -> Result<Vec<FileReview>, String> {
    let mut reviews = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut total_bytes: u64 = 0;
    while let Some(current_dir) = stack.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read directory '{:?}': {}", current_dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read a directory entry: {e}"))?;
            let path = entry.path();
            let rel_str = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_string();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to inspect '{rel_str}': {e}"))?;

            if meta.is_dir() {
                stack.push(path);
            } else if meta.file_type().is_symlink() {
                // A link carries no bytes of its own, so the manifest records the
                // target instead of pretending to hash content.
                let target = fs::read_link(&path)
                    .map(|t| t.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut hasher = Sha256::new();
                hasher.update(target.as_bytes());
                push_review(&reviews, &rel_str)?;
                reviews.push(FileReview {
                    path: rel_str,
                    sha256: format!("{:x}", hasher.finalize()),
                    bytes: 0,
                    executable: false,
                    content: Some(format!("symlink -> {}", target)),
                });
            } else if meta.is_file() {
                push_review(&reviews, &rel_str)?;
                let mut file = fs::File::open(&path)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;
                let digest = digest_stream(&mut file, MAX_FILE_BYTES)
                    .map_err(|e| format!("'{rel_str}' {e}"))?;
                total_bytes = total_bytes
                    .checked_add(digest.bytes)
                    .ok_or_else(|| format!("'{rel_str}' overflowed the snapshot size"))?;
                if total_bytes > MAX_TOTAL_BYTES {
                    return Err(format!(
                        "Snapshot of '{}' exceeds the maximum total size of {} bytes",
                        root.display(),
                        MAX_TOTAL_BYTES
                    ));
                }
                #[cfg(unix)]
                let is_exec = {
                    use std::os::unix::fs::PermissionsExt;
                    (meta.permissions().mode() & 0o111) != 0
                };
                #[cfg(not(unix))]
                let is_exec = false;
                reviews.push(FileReview {
                    path: rel_str,
                    sha256: digest.sha256,
                    bytes: digest.bytes,
                    executable: is_exec,
                    content: digest.preview,
                });
            }
        }
    }
    reviews.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(reviews)
}

fn push_review(reviews: &[FileReview], path: &str) -> Result<(), String> {
    if reviews.len() >= MAX_ENTRIES {
        return Err(format!(
            "'{path}' pushes the snapshot past {} entries, which no reviewer can read",
            MAX_ENTRIES
        ));
    }
    Ok(())
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
    let mut total_bytes: u64 = 0;

    let mut stack = vec![dir_path.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Failed to read directory '{:?}': {}", current_dir, e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read a directory entry: {e}"))?;
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

            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to inspect '{rel_str}': {e}"))?;
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
                push_review(&reviews, &rel_str)?;
                let mut file = fs::File::open(&path)
                    .map_err(|e| format!("Failed to read file '{}': {}", rel_str, e))?;
                let digest = digest_stream(&mut file, MAX_FILE_BYTES)
                    .map_err(|e| format!("'{rel_str}' {e}"))?;
                total_bytes = total_bytes
                    .checked_add(digest.bytes)
                    .ok_or_else(|| format!("'{rel_str}' overflowed the snapshot size"))?;
                if total_bytes > MAX_TOTAL_BYTES {
                    return Err(format!(
                        "Source directory '{}' exceeds the maximum snapshot size of {} bytes",
                        dir_path.display(),
                        MAX_TOTAL_BYTES
                    ));
                }

                #[cfg(unix)]
                let is_exec = {
                    use std::os::unix::fs::PermissionsExt;
                    (meta.permissions().mode() & 0o111) != 0
                };
                #[cfg(not(unix))]
                let is_exec = false;

                reviews.push(FileReview {
                    path: rel_str.clone(),
                    sha256: digest.sha256,
                    bytes: digest.bytes,
                    executable: is_exec,
                    content: digest.preview,
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

    // The review is hashed and displayed in order, so it must not depend on the
    // sequence the filesystem handed back.
    reviews.sort_by(|a, b| a.path.cmp(&b.path));

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

    // A 1 MiB request frame can compress a terabyte of content, so the
    // decompressed archive is bounded before a single entry is looked at.
    let tar_bytes = inflate_bounded(bytes)?;
    require_tar_terminator(&tar_bytes)?;
    let mut archive = tar::Archive::new(&tar_bytes[..]);

    let mut reviews = Vec::new();
    let mut total_bytes: u64 = 0;
    let entries = archive
        .entries()
        .map_err(|e| format!("Failed to parse tar archive entries: {}", e))?;

    for entry_res in entries {
        let mut entry = entry_res.map_err(|e| {
            format!("Failed to read archive entry (the archive may be truncated): {e}")
        })?;
        let path = entry
            .path()
            .map_err(|e| format!("Invalid path in archive entry: {}", e))?;
        let path_str = path.to_string_lossy().to_string();
        reject_unsafe_path(&path_str)?;

        let header = entry.header();
        if header.entry_type().is_dir() {
            // A directory is structure, not content; its path was checked above.
            continue;
        }
        if header.entry_type().is_symlink() {
            let target =
                link_target(&mut entry).map_err(|e| format!("Unreadable symlink entry: {e}"))?;
            reject_unsafe_link("symlink", &path_str, &target)?;
            push_review(&reviews, &path_str)?;
            let mut target_hasher = Sha256::new();
            target_hasher.update(target.as_bytes());
            reviews.push(FileReview {
                path: path_str,
                sha256: format!("{:x}", target_hasher.finalize()),
                bytes: 0,
                executable: false,
                content: Some(format!("symlink -> {}", target)),
            });
            continue;
        }
        if header.entry_type().is_hard_link() {
            let target =
                link_target(&mut entry).map_err(|e| format!("Unreadable hardlink entry: {e}"))?;
            reject_unsafe_link("hardlink", &path_str, &target)?;
            push_review(&reviews, &path_str)?;
            let mut target_hasher = Sha256::new();
            target_hasher.update(target.as_bytes());
            reviews.push(FileReview {
                path: path_str,
                sha256: format!("{:x}", target_hasher.finalize()),
                bytes: 0,
                executable: false,
                content: Some(format!("hardlink -> {}", target)),
            });
            continue;
        }
        if !header.entry_type().is_file() {
            // Devices, fifos and sockets are not content a review can show, and
            // unpacking them writes special files into the destination. Pax and
            // GNU extended headers are tar bookkeeping beside the real entries,
            // so they carry no risk and are skipped.
            if header.entry_type().is_character_special()
                || header.entry_type().is_block_special()
                || header.entry_type().is_fifo()
            {
                return Err(format!(
                    "Unsupported non-regular entry in the source archive: '{path_str}'"
                ));
            }
            continue;
        }

        // Trust the header only to fail fast; a lying size still cannot make this
        // read more than the cap because the stream is bounded as it goes.
        let declared = header
            .entry_size()
            .map_err(|e| format!("Unreadable entry size for '{path_str}': {e}"))?;
        if declared > MAX_FILE_BYTES {
            return Err(format!(
                "'{path_str}' declares {declared} bytes, over the snapshot limit of {MAX_FILE_BYTES}"
            ));
        }
        let mode = header.mode().unwrap_or(0);
        push_review(&reviews, &path_str)?;

        let mut reader = entry.take(MAX_FILE_BYTES + 1);
        let digest =
            digest_stream(&mut reader, MAX_FILE_BYTES).map_err(|e| format!("'{path_str}' {e}"))?;
        total_bytes = total_bytes
            .checked_add(digest.bytes)
            .ok_or_else(|| format!("'{path_str}' overflowed the snapshot size"))?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "Archive exceeds the maximum snapshot size of {} bytes",
                MAX_TOTAL_BYTES
            ));
        }

        reviews.push(FileReview {
            path: path_str,
            sha256: digest.sha256,
            bytes: digest.bytes,
            executable: (mode & 0o111) != 0,
            content: digest.preview,
        });
    }

    reviews.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(SourceSnapshot {
        tarball_bytes: bytes.to_vec(),
        sha256_hash,
        reviews,
    })
}
