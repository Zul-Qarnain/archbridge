//! Source snapshotting: what a review may contain, and what an untrusted
//! archive must never be able to do. These cases build archives in memory, so
//! they run offline and on any host.

use archbridge::archive::{snapshot_directory, snapshot_tarball_bytes, walk_tree, FileReview};
use flate2::write::GzEncoder;
use std::io::Write;

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// One entry of a fixture archive, written straight into ustar bytes so a name
/// or header that `tar::Builder` would refuse can still be tested.
struct Raw {
    name: String,
    data: Vec<u8>,
    typeflag: u8,
    linkname: String,
    declared_size: Option<u64>,
}

impl Raw {
    fn file(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            data: data.to_vec(),
            typeflag: b'0',
            linkname: String::new(),
            declared_size: None,
        }
    }
    fn symlink(name: &str, target: &str) -> Self {
        Self {
            name: name.to_string(),
            data: Vec::new(),
            typeflag: b'2',
            linkname: target.to_string(),
            declared_size: None,
        }
    }
    fn hardlink(name: &str, target: &str) -> Self {
        Self {
            name: name.to_string(),
            data: Vec::new(),
            typeflag: b'1',
            linkname: target.to_string(),
            declared_size: None,
        }
    }
    fn fifo(name: &str) -> Self {
        Self {
            name: name.to_string(),
            data: Vec::new(),
            typeflag: b'6',
            linkname: String::new(),
            declared_size: None,
        }
    }
    fn directory(name: &str) -> Self {
        Self {
            name: name.to_string(),
            data: Vec::new(),
            typeflag: b'5',
            linkname: String::new(),
            declared_size: None,
        }
    }
    /// A header that claims more bytes than the snapshot limit allows.
    fn giant(name: &str) -> Self {
        Self {
            name: name.to_string(),
            data: b"x".to_vec(),
            typeflag: b'0',
            linkname: String::new(),
            declared_size: Some(MAX_FILE_BYTES + 1),
        }
    }
}

fn header_block(entry: &Raw) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    let set = |field: (usize, usize), value: &[u8], block: &mut Vec<u8>| {
        let width = field.1 - field.0;
        let taken = &value[..width.min(value.len())];
        block[field.0..field.0 + taken.len()].copy_from_slice(taken);
    };
    // The name field is 100 bytes; a fixture longer than that is truncated,
    // which is exactly what a hostile archive can do to a naive reader.
    set((0, 100), entry.name.as_bytes(), &mut block);
    set((100, 108), b"0000644", &mut block);
    set((108, 116), b"0000000", &mut block);
    set((116, 124), b"0000000", &mut block);
    let size = entry.declared_size.unwrap_or(entry.data.len() as u64);
    set((124, 136), format!("{size:011o}").as_bytes(), &mut block);
    set((136, 148), b"00000000000", &mut block);
    block[156] = entry.typeflag;
    set((157, 257), entry.linkname.as_bytes(), &mut block);
    set((257, 263), b"ustar\0", &mut block);
    set((263, 265), b"00", &mut block);
    // The checksum covers the whole block with its own field treated as spaces.
    block[148..156].fill(b' ');
    let sum: u32 = block.iter().map(|byte| *byte as u32).sum();
    set((148, 156), format!("{sum:06o}\0 ").as_bytes(), &mut block);
    block
}

fn raw_tar(entries: &[Raw]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(&header_block(entry));
        if entry.typeflag == b'0' {
            bytes.extend_from_slice(&entry.data);
            let pad = (512 - entry.data.len() % 512) % 512;
            bytes.extend(std::iter::repeat_n(0u8, pad));
        }
    }
    // Two zero blocks terminate the archive.
    bytes.extend_from_slice(&[0u8; 1024]);
    bytes
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip write");
    encoder.finish().expect("gzip finish")
}

fn build(entries: &[Raw]) -> Vec<u8> {
    gzip(&raw_tar(entries))
}

fn names(reviews: &[FileReview]) -> Vec<&str> {
    reviews.iter().map(|review| review.path.as_str()).collect()
}

#[test]
fn an_ordinary_archive_snapshots_with_paths_sizes_and_previews() {
    let archive = build(&[
        Raw::file("proj/README.md", b"hello"),
        Raw::file("proj/src/main.c", b"int main(void){return 0;}"),
        Raw::symlink("proj/README", "README.md"),
        Raw::directory("proj/empty/"),
    ]);
    let snapshot = snapshot_tarball_bytes(&archive).expect("snapshot");
    assert_eq!(
        names(&snapshot.reviews),
        vec!["proj/README", "proj/README.md", "proj/src/main.c"],
        "a directory entry carries no content and must not be listed"
    );
    let link = &snapshot.reviews[0];
    assert_eq!(link.content.as_deref(), Some("symlink -> README.md"));
    assert_eq!(link.bytes, 0);
    assert_eq!(
        snapshot.reviews[1].content.as_deref(),
        Some("hello"),
        "small text files are shown to the reviewer"
    );
    assert_eq!(snapshot.reviews[1].bytes, 5);
    assert_eq!(snapshot.sha256_hash.len(), 64);

    // The pinned bytes are the bytes that were handed in, unchanged.
    use sha2::Digest;
    let mut rehash = sha2::Sha256::new();
    rehash.update(&archive);
    assert_eq!(
        snapshot.sha256_hash,
        format!("{:x}", rehash.finalize()),
        "the hash must be of the exact bytes"
    );
}

#[test]
fn paths_that_escape_the_tree_are_refused() {
    for escaping in [
        "../evil",
        "/etc/passwd",
        "ok/../../evil",
        "./../../evil",
        "deep/nested/../../../../evil",
    ] {
        let archive = build(&[Raw::file("good.txt", b"ok"), Raw::file(escaping, b"bad")]);
        let error = snapshot_tarball_bytes(&archive)
            .err_or_panic(format!("'{escaping}' was accepted into a snapshot"));
        assert!(error.contains("Unsafe path"), "{escaping}: {error}");
    }
}

#[test]
fn a_name_merely_containing_two_dots_is_not_traversal() {
    // `contains("..")` used to reject legitimate files such as `a..b` and
    // `v1.2..tar.gz`, which made honest sources unsnapshotable.
    let archive = build(&[Raw::file("notes..draft.txt", b"ok")]);
    let snapshot = snapshot_tarball_bytes(&archive).expect("legitimate name accepted");
    assert_eq!(names(&snapshot.reviews), vec!["notes..draft.txt"]);
}

#[test]
fn links_that_point_outside_the_tree_are_refused() {
    for target in ["/etc/passwd", "../../etc/shadow", "a/../../x"] {
        let archive = build(&[Raw::file("good.txt", b"ok"), Raw::symlink("link", target)]);
        let error = snapshot_tarball_bytes(&archive)
            .err_or_panic(format!("symlink to '{target}' was accepted"));
        assert!(error.contains("Unsafe symlink"), "{target}: {error}");
    }

    // A link inside the tree is fine and is shown as what it is.
    let archive = build(&[
        Raw::file("dir/file.txt", b"ok"),
        Raw::symlink("dir/nearby.txt", "file.txt"),
        Raw::symlink("dir/inner.txt", "../file.txt"),
        Raw::hardlink("dir/other.txt", "file.txt"),
    ]);
    let snapshot = snapshot_tarball_bytes(&archive).expect("in-tree links");
    assert_eq!(
        names(&snapshot.reviews),
        vec![
            "dir/file.txt",
            "dir/hardlink-unused",
            "dir/inner.txt",
            "dir/nearby.txt",
            "dir/other.txt"
        ]
        .into_iter()
        .filter(|name| *name != "dir/hardlink-unused")
        .collect::<Vec<_>>(),
        "links inside the tree are recorded, not dropped"
    );

    // A hardlink that reaches outside the tree is refused like a symlink.
    let archive = build(&[
        Raw::file("dir/file.txt", b"ok"),
        Raw::hardlink("dir/sneaky", "../../etc/shadow"),
    ]);
    let error = snapshot_tarball_bytes(&archive).err_or_panic("an escaping hardlink was accepted");
    assert!(error.contains("Unsafe hardlink"), "{error}");
}

#[test]
fn special_files_are_refused_rather_than_recorded_as_content() {
    let archive = build(&[Raw::file("good.txt", b"ok"), Raw::fifo("pipe")]);
    let error =
        snapshot_tarball_bytes(&archive).err_or_panic("a fifo entry was accepted into a snapshot");
    assert!(error.contains("non-regular entry"), "{error}");
}

#[test]
fn a_header_claiming_a_huge_file_fails_before_any_bulk_read() {
    let archive = build(&[Raw::giant("bomb.bin")]);
    let error =
        snapshot_tarball_bytes(&archive).err_or_panic("a 256 MiB declared entry was accepted");
    assert!(error.contains("declares"), "{error}");
    assert!(error.contains("limit"), "{error}");
}

#[test]
fn truncated_and_foreign_archives_error_instead_of_panicking() {
    let raw = raw_tar(&[
        Raw::file("a/one.txt", b"first"),
        Raw::file("a/two.txt", b"second"),
    ]);

    // 4 x 512 header/data blocks: everything before that offset is real content,
    // and the bytes after it are the zero terminator, whose tail may be trimmed
    // without losing a file.
    let content_end = 2048;
    for cut in 0..content_end {
        let archive = gzip(&raw[..cut]);
        let outcome = snapshot_tarball_bytes(&archive);
        match outcome {
            Ok(snapshot) => panic!(
                "a truncated archive at byte {cut} was accepted as complete: {:?}",
                names(&snapshot.reviews)
            ),
            Err(error) => assert!(
                error.contains("truncated")
                    || error.contains("decompress")
                    || error.contains("read"),
                "at byte {cut}: {error}"
            ),
        }
    }

    // The same bytes with the terminator intact are accepted.
    let archive = gzip(&raw);
    let snapshot = snapshot_tarball_bytes(&archive).expect("a whole archive");
    assert_eq!(names(&snapshot.reviews), vec!["a/one.txt", "a/two.txt"]);

    // Cutting the compressed bytes instead must not panic either.
    let archive = gzip(&raw);
    for cut in 0..archive.len() {
        let _ = std::panic::catch_unwind(|| snapshot_tarball_bytes(&archive[..cut]))
            .unwrap_or_else(|_| panic!("panic on a truncated gzip stream at byte {cut}"));
    }

    for garbage in [
        vec![0u8; 4096],
        b"not an archive at all".to_vec(),
        vec![0xffu8; 512],
    ] {
        let outcome = std::panic::catch_unwind(|| snapshot_tarball_bytes(&garbage))
            .unwrap_or_else(|_| panic!("snapshot panicked on garbage input"));
        assert!(
            outcome.is_err(),
            "input that is not a gzip tar was accepted: {garbage:?}"
        );
    }
}

#[test]
fn an_entry_bomb_is_stopped_at_the_review_limit() {
    // 20 001 paths is a review nobody can read and a memory sink; the snapshot
    // must refuse it rather than list every one.
    let many: Vec<Raw> = (0..20_001)
        .map(|index| Raw::file(&format!("file-{index}.txt"), b"x"))
        .collect();
    let archive = build(&many);
    let error = snapshot_tarball_bytes(&archive).err_or_panic("20 001 entries were reviewed");
    assert!(error.contains("entries"), "{error}");
}

#[test]
fn directory_snapshots_stream_and_bound_what_they_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    std::fs::write(dir.path().join("src/main.c"), b"int main(){}").expect("write");
    std::fs::write(dir.path().join("README"), b"readme").expect("write");

    let snapshot = snapshot_directory(dir.path()).expect("snapshot");
    assert_eq!(
        names(&snapshot.reviews),
        vec!["README", "src/main.c"],
        "{:?}",
        names(&snapshot.reviews)
    );
    // The tarball round-trips to the same review, which is what pinned bytes mean.
    let from_bytes = snapshot_tarball_bytes(&snapshot.tarball_bytes).expect("re-snapshot");
    assert_eq!(
        names(&from_bytes.reviews),
        names(&snapshot.reviews),
        "a directory snapshot must re-read as itself"
    );
    for review in &snapshot.reviews {
        let match_ = from_bytes
            .reviews
            .iter()
            .find(|other| other.path == review.path)
            .expect("same path");
        assert_eq!(match_.sha256, review.sha256, "hash drifted");
    }
}

#[test]
fn a_directory_too_large_to_review_is_an_error_not_a_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A sparse file: no disk is used, but a whole-file read would still walk
    // past the cap.
    let big = dir.path().join("artifact.bin");
    let file = std::fs::File::create(&big).expect("create");
    file.set_len(MAX_FILE_BYTES + 64 * 1024).expect("sparse");

    let error = snapshot_directory(dir.path()).err_or_panic("an over-cap file was snapshotted");
    assert!(error.contains("artifact.bin"), "{error}");
    assert!(
        error.contains("maximum snapshot file size"),
        "the message must say which limit was hit: {error}"
    );

    // walk_tree, which reviews an extracted chroot payload, refuses it too.
    let error = walk_tree(dir.path()).err_or_panic("walk_tree accepted an over-cap file");
    assert!(error.contains("maximum snapshot file size"), "{error}");
}

#[test]
fn binary_and_oversized_content_is_never_offered_as_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("blob"), [0u8; 8]).expect("write");
    std::fs::write(dir.path().join("big.txt"), vec![b'a'; 20_000]).expect("write");
    let reviews = walk_tree(dir.path()).expect("walk");
    for review in &reviews {
        assert!(review.content.is_none(), "{:?} leaked content", review.path);
    }
    let big = reviews
        .iter()
        .find(|review| review.path == "big.txt")
        .expect("big.txt");
    assert_eq!(big.bytes, 20_000, "the size still has to be exact");
}

/// Deterministic sweep of hostile entry names: the snapshot either refuses the
/// path or reports it, and never panics or rewrites it into something else.
#[test]
fn fuzz_entry_names_never_panic() {
    let mut hostile: Vec<String> = vec![
        "",
        ".",
        "..",
        "/",
        "//",
        "./",
        "../",
        "a/../../b",
        "....//etc/passwd",
        "\u{0}",
        "a\u{0}b",
        "\u{1f600}.txt",
        "-x",
        "$(id)",
        "`id`",
        "a b/c d",
        "very",
        "/absolute/deep/path",
        "C:\\Windows\\evil",
        "a/./b",
        "./a",
        "x/",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    hostile.push("d".repeat(300));
    hostile.push(format!("prefix/{}", "../x".repeat(40)));

    for name in &hostile {
        let archive = build(&[Raw::file(name, b"payload")]);
        let outcome = std::panic::catch_unwind(|| snapshot_tarball_bytes(&archive));
        let outcome = outcome.unwrap_or_else(|_| panic!("panic on entry name {name:?}"));
        match outcome {
            Ok(snapshot) => {
                for review in &snapshot.reviews {
                    assert!(
                        !review.path.is_empty(),
                        "an empty path was recorded for {name:?}"
                    );
                    assert!(
                        !review.path.starts_with('/'),
                        "an absolute path was recorded for {name:?}"
                    );
                    assert!(
                        !review.path.contains('\0'),
                        "a NUL survived into a recorded path for {name:?}"
                    );
                    assert!(
                        !review.path.split('/').any(|segment| segment == ".."),
                        "a traversal segment was recorded for {name:?}: {}",
                        review.path
                    );
                }
            }
            Err(error) => assert!(
                error.contains("Unsafe")
                    || error.contains("Unsupported")
                    || error.contains("parse")
                    || error.contains("Invalid")
                    || error.contains("Unreadable")
                    || error.contains("names nothing"),
                "unhelpful rejection for {name:?}: {error}"
            ),
        }
    }
}

/// Several cases assert on the rejection text itself, so take the error arm and
/// fail loudly when something that must be refused slipped through.
trait Rejected {
    fn err_or_panic<M: std::fmt::Display>(self, message: M) -> String;
}

impl<T> Rejected for Result<T, String> {
    fn err_or_panic<M: std::fmt::Display>(self, message: M) -> String {
        match self {
            Err(error) => error,
            Ok(_) => panic!("{message}"),
        }
    }
}

// ------------------------------------------------- directory snapshot selection

fn tar_names_and_links(tarball: &[u8]) -> Vec<(String, Option<String>)> {
    let decoder = flate2::read::GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            let link = entry
                .link_name()
                .unwrap()
                .map(|l| l.to_string_lossy().to_string());
            (path, link)
        })
        .collect()
}

#[test]
fn only_top_level_vcs_and_build_output_are_left_out_of_a_directory_snapshot() {
    // Regression: `starts_with(".git")` / `contains("target/")` matched on the
    // whole relative path, so `.github/`, `.gitignore` and any `…/target/…`
    // source directory vanished from the review and the tarball.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        ("main.c", "int main(){}"),
        (".gitignore", "build/\n"),
        (".github/workflows/ci.yml", "on: push\n"),
        ("src/target/backend.c", "void b(){}"),
        ("include/mytarget/x.h", "#pragma once"),
        ("notes..txt", "two dots in a name"),
        (".git/config", "[core]"),
        ("target/debug/out", "binary"),
        (".archbridge/jobs/x/log", "log"),
    ] {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    let snapshot = archbridge::archive::snapshot_directory(dir.path()).unwrap();
    let reviewed: Vec<&str> = snapshot.reviews.iter().map(|r| r.path.as_str()).collect();
    for kept in [
        "main.c",
        ".gitignore",
        ".github/workflows/ci.yml",
        "src/target/backend.c",
        "include/mytarget/x.h",
        "notes..txt",
    ] {
        assert!(reviewed.contains(&kept), "{kept} was dropped: {reviewed:?}");
    }
    for skipped in [".git/config", "target/debug/out", ".archbridge/jobs/x/log"] {
        assert!(!reviewed.contains(&skipped), "{skipped} must stay out");
    }
}

#[cfg(unix)]
#[test]
fn symlinks_that_stay_inside_the_tree_are_archived_and_reviewed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/sub")).unwrap();
    std::fs::write(dir.path().join("lib/libx.so.1"), "elf").unwrap();
    std::fs::write(dir.path().join("lib/data.txt"), "d").unwrap();
    std::os::unix::fs::symlink("libx.so.1", dir.path().join("lib/libx.so")).unwrap();
    std::os::unix::fs::symlink("../data.txt", dir.path().join("lib/sub/data")).unwrap();
    std::os::unix::fs::symlink("libx..so.1", dir.path().join("lib/odd")).unwrap();

    let snapshot = archbridge::archive::snapshot_directory(dir.path()).unwrap();
    let entries = tar_names_and_links(&snapshot.tarball_bytes);
    assert!(
        entries.contains(&("lib/libx.so".to_string(), Some("libx.so.1".to_string()))),
        "the link was silently dropped from the archive: {entries:?}"
    );
    assert!(entries
        .iter()
        .any(|(p, l)| p == "lib/sub/data" && l.as_deref() == Some("../data.txt")));
    let review = snapshot
        .reviews
        .iter()
        .find(|r| r.path == "lib/libx.so")
        .expect("the link is part of the review");
    assert_eq!(review.content.as_deref(), Some("symlink -> libx.so.1"));
}

#[cfg(unix)]
#[test]
fn symlinks_that_leave_the_tree_are_still_refused_with_a_readable_message() {
    for (name, target) in [
        ("up", "../outside"),
        ("deep", "../../etc/passwd"),
        ("abs", "/etc/passwd"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target, dir.path().join(name)).unwrap();
        let Err(error) = archbridge::archive::snapshot_directory(dir.path()) else {
            panic!("{target} must be refused");
        };
        assert!(error.contains("Unsafe symlink"), "{target}: {error}");
        assert!(error.contains("source directory"), "{error}");
        assert!(!error.contains("'\""), "no Debug-quoted target: {error}");
    }
}

#[test]
fn a_missing_directory_error_does_not_double_quote_the_path() {
    let Err(error) =
        archbridge::archive::snapshot_directory(std::path::Path::new("/nonexistent/x"))
    else {
        panic!("a missing directory must be refused");
    };
    assert!(error.contains("'/nonexistent/x'"), "{error}");
}
