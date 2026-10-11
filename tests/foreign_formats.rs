//! Foreign-package inspection against fixtures assembled here.
//!
//! Every case is data-only: the fixtures carry maintainer scripts and RPM
//! scriptlets, and the tests assert they are reported as unexecuted text. The
//! archive fixtures are built with `bsdtar`, the same tool the inspection path
//! uses, so a host without it skips instead of failing for the wrong reason.

use archbridge::elf::build_fixture_object;
use archbridge::inspect::inspect_package;
use archbridge::rpm::{self, FixtureTag};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bsdtar_available() -> bool {
    Command::new("bsdtar")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn skip_without_bsdtar(what: &str) -> bool {
    if bsdtar_available() {
        true
    } else {
        eprintln!("skipping {what}: bsdtar is not installed on this host");
        false
    }
}

/// `bsdtar -c[z]f out -C dir .` — the members come out rooted at `./`, the way
/// a real package stores them.
fn tar_dir(out: &Path, dir: &Path, gzip: bool) {
    let flag = if gzip { "-czf" } else { "-cf" };
    let status = Command::new("bsdtar")
        .args([
            flag,
            &out.display().to_string(),
            "-C",
            &dir.display().to_string(),
            ".",
        ])
        .status()
        .expect("bsdtar should run");
    assert!(status.success(), "failed to build fixture archive {out:?}");
}

/// A `file(1)`-style `ar` container, which is what a `.deb` is.
fn ar_archive(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name,
            0,
            0,
            0,
            "100644",
            data.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

fn stage(dir: &Path, relative: &str, bytes: &[u8]) {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if relative.contains("/bin/") {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// A DEB whose payload ships two ELF objects linking against different
/// libraries, one text file, a desktop entry and a systemd unit.
fn build_deb(dir: &Path) -> PathBuf {
    let control = dir.join("control");
    stage(
        &control,
        "control",
        b"Package: hello
Version: 2.10-3
Architecture: amd64
Maintainer: Someone <s@example.test>
Depends: libc6 (>= 2.34), libz
Provides: hello-doc
Description: Say hello
 Builds a greeting.
",
    );
    stage(
        &control,
        "postinst",
        b"#!/bin/sh\necho this would run on install\n",
    );
    stage(&control, "postrm", b"rm -rf /var/lib/hello\n");
    let control_tar = dir.join("control.tar.gz");
    tar_dir(&control_tar, &control, true);

    let data = dir.join("data");
    stage(
        &data,
        "usr/bin/hello",
        &build_fixture_object(true, true, true, true, &["libc.so.6", "libz.so.1"]),
    );
    stage(
        &data,
        "usr/lib/libgreet.so.1",
        &build_fixture_object(true, true, true, true, &["libc.so.6"]),
    );
    stage(&data, "usr/share/doc/hello/README.md", b"# hello\n");
    stage(
        &data,
        "usr/share/applications/hello.desktop",
        b"[Desktop Entry]\nName=hello\n",
    );
    stage(
        &data,
        "usr/lib/systemd/system/hello.service",
        b"[Unit]\nDescription=hello\n",
    );
    let data_tar = dir.join("data.tar.gz");
    tar_dir(&data_tar, &data, true);

    let deb = dir.join("hello_2.10-3_amd64.deb");
    fs::write(
        &deb,
        ar_archive(&[
            ("debian-binary", b"2.0\n".to_vec()),
            ("control.tar.gz", fs::read(&control_tar).unwrap()),
            ("data.tar.gz", fs::read(&data_tar).unwrap()),
        ]),
    )
    .unwrap();
    deb
}

#[test]
fn deb_report_names_the_libraries_its_payload_links_against() {
    if !skip_without_bsdtar("deb payload scan") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deb = build_deb(dir.path());
    let report = inspect_package(&deb.display().to_string()).expect("fixture deb should inspect");

    assert_eq!(report.format, "deb");
    assert_eq!(report.package_name, "hello");
    assert_eq!(report.version, "2.10-3");
    assert_eq!(report.architecture, "amd64");
    assert_eq!(report.maintainer, "Someone <s@example.test>");
    assert_eq!(
        report.dependencies,
        vec!["libc6 (>= 2.34)".to_string(), "libz".to_string()]
    );
    assert_eq!(report.provides, vec!["hello-doc".to_string()]);
    assert_eq!(report.desktop_files, vec!["hello.desktop".to_string()]);
    assert_eq!(report.systemd_units, vec!["hello.service".to_string()]);
    assert_eq!(
        report.needed_libraries,
        vec!["libc.so.6".to_string(), "libz.so.1".to_string()],
        "the union of what the payload's own ELF objects declare, deduped and sorted"
    );
    assert_eq!(report.size_bytes, fs::metadata(&deb).unwrap().len());
    assert_eq!(report.sha256.len(), 64);
}

#[test]
fn deb_scriptlets_are_reported_as_unexecuted_text() {
    if !skip_without_bsdtar("deb script review") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deb = build_deb(dir.path());
    let report = inspect_package(&deb.display().to_string()).expect("fixture deb should inspect");

    let names: Vec<&str> = report
        .maintainer_scripts
        .iter()
        .map(|script| script.name.as_str())
        .collect();
    assert_eq!(names, vec!["postinst", "postrm"]);
    for script in &report.maintainer_scripts {
        assert!(
            !script.executed,
            "{} must never be marked executed",
            script.name
        );
        assert_eq!(script.status, "readable_shell");
        assert!(!script.content.is_empty());
    }
    assert!(
        report
            .compatibility_note
            .contains("2 maintainer scripts parsed but unexecuted"),
        "{}",
        report.compatibility_note
    );
    assert!(report.compatibility_note.contains("2 shared library needs"));
}

/// A header carrying the tags a real spec writes, including one of every
/// scriptlet phase and a `%post` with its own interpreter.
fn fixture_tags() -> Vec<FixtureTag> {
    vec![
        FixtureTag::text(rpm::TAG_NAME, "hello"),
        FixtureTag::text(rpm::TAG_VERSION, "2.10"),
        FixtureTag::text(rpm::TAG_RELEASE, "3"),
        FixtureTag::text(rpm::TAG_ARCH, "x86_64"),
        FixtureTag::text(rpm::TAG_SUMMARY, "Say hello"),
        FixtureTag::text(rpm::TAG_DESCRIPTION, "Builds a greeting on demand."),
        FixtureTag::text(rpm::TAG_LICENSE, "GPLv3+"),
        FixtureTag::text(rpm::TAG_URL, "https://example.test/hello"),
        FixtureTag::text(rpm::TAG_PACKAGER, "Packager <p@example.test>"),
        FixtureTag::list(
            rpm::TAG_REQUIRENAME,
            &[
                "glibc",
                "libfoo",
                "bash",
                "rpmlib(CompressedFileNames)",
                "config(/etc/hello.conf)",
            ],
        ),
        FixtureTag::ints(
            rpm::TAG_REQUIREFLAGS,
            &[
                rpm::SENSE_EQUAL,
                rpm::SENSE_GREATER | rpm::SENSE_EQUAL,
                0,
                rpm::SENSE_RPMLIB,
                rpm::SENSE_CONFIG,
            ],
        ),
        FixtureTag::list(
            rpm::TAG_REQUIREVERSION,
            &["2.34", "1.2", "", "3.0.4", "2.17"],
        ),
        FixtureTag::list(rpm::TAG_PROVIDENAME, &["hello", "hello-doc"]),
        FixtureTag::text(rpm::TAG_PAYLOADFORMAT, "cpio"),
        FixtureTag::text(rpm::TAG_PAYLOADCOMPRESSOR, "zstd"),
        FixtureTag::text(rpm::TAG_PREIN, "echo preinst\n"),
        FixtureTag::text(rpm::TAG_POSTIN, "/usr/bin/mkdir -p /var/lib/hello\n"),
        FixtureTag::text(rpm::TAG_POSTINPROG, "/usr/bin/lua"),
        FixtureTag::text(rpm::TAG_PREUN, "rm -f /var/lib/hello/state\n"),
        FixtureTag::text(rpm::TAG_POSTUN, "echo postun\n"),
        FixtureTag::text(rpm::TAG_PRETRANS, "echo pretrans\n"),
        FixtureTag::text(rpm::TAG_POSTTRANS, "echo posttrans\n"),
    ]
}

fn build_rpm(dir: &Path) -> PathBuf {
    let path = dir.join("hello-2.10-3.x86_64.rpm");
    fs::write(&path, rpm::build_fixture_package(&fixture_tags())).unwrap();
    path
}

#[test]
fn rpm_metadata_comes_out_of_the_header_not_the_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let rpm = build_rpm(dir.path());
    // The file name carries a different version entirely, so anything read from
    // it would show up in the assertion.
    fs::rename(&rpm, dir.path().join("mislabelled-9.9-9.i686.rpm")).unwrap();
    let renamed = dir.path().join("mislabelled-9.9-9.i686.rpm");
    let report =
        inspect_package(&renamed.display().to_string()).expect("fixture rpm should inspect");

    assert_eq!(report.format, "rpm");
    assert_eq!(report.package_name, "hello");
    assert_eq!(report.version, "2.10-3");
    assert_eq!(report.architecture, "x86_64");
    assert_eq!(report.maintainer, "Packager <p@example.test>");
    assert!(report.description.contains("Say hello"));
    assert!(report.description.contains("Builds a greeting on demand."));
    assert_eq!(
        report.dependencies,
        vec![
            "glibc (= 2.34)".to_string(),
            "libfoo (>= 1.2)".to_string(),
            "bash".to_string(),
        ],
        "rpmlib() capabilities and config() files name nothing a pacman install could satisfy"
    );
    assert_eq!(report.provides, vec!["hello", "hello-doc"]);
    assert!(report.compatibility_note.contains("GPLv3+"));
    assert!(report
        .compatibility_note
        .contains("https://example.test/hello"));
    assert!(report.needed_libraries.is_empty());
    assert!(
        report.compatibility_note.contains("0 shared library needs"),
        "{}",
        report.compatibility_note
    );
}

#[test]
fn rpm_scriptlets_are_surfaced_like_deb_maintainer_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let rpm = build_rpm(dir.path());
    let report = inspect_package(&rpm.display().to_string()).expect("fixture rpm should inspect");

    let names: Vec<&str> = report
        .maintainer_scripts
        .iter()
        .map(|script| script.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "prein",
            "postin",
            "preun",
            "postun",
            "pretrans",
            "posttrans"
        ],
        "scriptlets are listed in transaction order"
    );
    for script in &report.maintainer_scripts {
        assert!(
            !script.executed,
            "{} must never be marked executed",
            script.name
        );
        assert!(!script.content.is_empty());
    }
    assert_eq!(report.maintainer_scripts[0].status, "readable_shell");
    assert_eq!(
        report.maintainer_scripts[1].status, "interpreter:/usr/bin/lua",
        "a scriptlet with its own interpreter must not look like a plain shell block"
    );
    assert!(report
        .compatibility_note
        .contains("6 scriptlets parsed but unexecuted"));
    assert!(report.compatibility_note.contains("/usr/bin/lua"));
}

#[test]
fn a_malformed_rpm_is_an_error_rather_than_a_hollow_report() {
    let dir = tempfile::tempdir().unwrap();
    let good = rpm::build_fixture_package(&fixture_tags());

    let truncated = dir.path().join("cut-1.0-1.x86_64.rpm");
    fs::write(&truncated, &good[..good.len() / 2]).unwrap();
    let error = inspect_package(&truncated.display().to_string()).unwrap_err();
    assert!(error.contains("RPM"), "{error}");

    let junk = dir.path().join("junk-1.0-1.x86_64.rpm");
    // Long enough to clear the lead check, so the magic is what rejects it.
    fs::write(&junk, vec![b'x'; 120]).unwrap();
    assert!(inspect_package(&junk.display().to_string())
        .unwrap_err()
        .contains("Not an RPM file"));

    // A header whose counters claim more than the file holds must be refused,
    // not allocated from.
    let inflated = dir.path().join("inflated-1.0-1.x86_64.rpm");
    let mut bytes = good.clone();
    let header_at = second_header(&bytes);
    bytes[header_at + 8..header_at + 12].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    fs::write(&inflated, &bytes).unwrap();
    let error = inspect_package(&inflated.display().to_string()).unwrap_err();
    assert!(
        error.contains("ceiling") || error.contains("region"),
        "{error}"
    );
}

/// Where the main header starts: the second region magic after the lead.
fn second_header(bytes: &[u8]) -> usize {
    let magic = [0x8e, 0xad, 0xe8];
    let signature = bytes
        .windows(3)
        .position(|window| window == magic)
        .expect("a fixture always carries a signature header");
    let rest = &bytes[signature + 16..];
    signature
        + 16
        + rest
            .windows(3)
            .position(|window| window == magic)
            .expect("a fixture always carries a main header")
}

/// This crate's own fixture object is written by the same author as its reader,
/// so it cannot prove the reader understands a real linker's output. Where the
/// host has `readelf`, the two answers are compared directly; the binary under
/// test is the test executable itself, which is always present and is a real
/// dynamically linked object.
#[test]
fn needed_libraries_match_what_readelf_reads() {
    let exe = std::env::current_exe().unwrap();
    let bytes = fs::read(&exe).unwrap();
    if bytes[..4] != [0x7f, b'E', b'L', b'F'] {
        eprintln!("skipping the readelf cross-check: {exe:?} is not an ELF object");
        return;
    }
    let ours = archbridge::elf::needed_libraries_in_file(&exe);
    match Command::new("readelf")
        .args(["-d", &exe.display().to_string()])
        .output()
    {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut theirs: Vec<String> = text
                .lines()
                .filter_map(|line| {
                    let (_, tail) = line.split_once("Shared library: [")?;
                    Some(tail.trim_end_matches(']').to_string())
                })
                .collect();
            theirs.sort();
            let mut ours_sorted = ours.clone();
            ours_sorted.sort();
            assert_eq!(
                ours_sorted, theirs,
                "the reader and readelf disagree about what {:?} needs",
                exe
            );
        }
        _ => {
            for name in &ours {
                assert!(
                    name.contains(".so") || name.starts_with("ld-"),
                    "library names should look like sonames, got {name:?}"
                );
            }
            eprintln!("skipping the readelf cross-check: readelf is not installed");
        }
    }
}

#[test]
fn deb_reports_scripts_dependencies_units_and_elf_without_execution() {
    if !skip_without_bsdtar("deb end to end inspection") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deb = build_deb(dir.path());
    let report = inspect_package(&deb.display().to_string()).expect("fixture deb should inspect");
    assert_eq!(report.format, "deb");
    for script in &report.maintainer_scripts {
        assert!(!script.executed);
    }
    assert!(!report.dependencies.is_empty());
    assert!(!report.systemd_units.is_empty());
    assert!(!report.needed_libraries.is_empty());
}

#[test]
fn real_rpm_inspection_never_claims_installability() {
    let path = std::env::var("ARCHBRIDGE_TEST_RPM").ok();
    let Some(path) = path else {
        eprintln!("skipping real rpm inspection: set ARCHBRIDGE_TEST_RPM=/path/to/file.rpm");
        return;
    };
    let report = match inspect_package(&path) {
        Ok(report) => report,
        // A host can hold a variable pointing at something this reader cannot
        // open; that is the error path, and it must be reported, not faked.
        Err(error) => panic!("real RPM {path} failed to inspect: {error}"),
    };
    assert_eq!(report.format, "rpm");
    assert!(!report.package_name.is_empty());
    for script in &report.maintainer_scripts {
        assert!(!script.executed);
    }
    assert!(
        !report.compatibility_note.contains("installable"),
        "inspection must never promise that a foreign package installs"
    );
}

/// A deb whose control file is exactly `control`.
fn deb_with_control(dir: &Path, control: &[u8]) -> PathBuf {
    let staged = dir.join("control-src");
    stage(&staged, "control", control);
    let control_tar = dir.join("control.tar.gz");
    tar_dir(&control_tar, &staged, true);
    let data = dir.join("data-src");
    stage(&data, "usr/share/doc/x/README", b"x\n");
    let data_tar = dir.join("data.tar.gz");
    tar_dir(&data_tar, &data, true);
    let deb = dir.join("x.deb");
    fs::write(
        &deb,
        ar_archive(&[
            ("debian-binary", b"2.0\n".to_vec()),
            ("control.tar.gz", fs::read(&control_tar).unwrap()),
            ("data.tar.gz", fs::read(&data_tar).unwrap()),
        ]),
    )
    .unwrap();
    deb
}

#[test]
fn a_description_continuation_line_cannot_overwrite_a_control_field() {
    if !skip_without_bsdtar("deb control parsing") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deb = deb_with_control(
        dir.path(),
        b"Package: real\nVersion: 1.0\nArchitecture: amd64\nDescription: tool\n Version: 99.9\n Depends: evil-package\n\tPackage: other\n",
    );
    let report = inspect_package(&deb.display().to_string()).unwrap();
    assert_eq!(report.package_name, "real");
    assert_eq!(report.version, "1.0", "a continuation line is not a field");
    assert!(report.dependencies.is_empty(), "{:?}", report.dependencies);
}

#[test]
fn a_file_that_is_not_a_debian_package_is_an_error_not_an_unknown_report() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("empty.deb", Vec::new()),
        ("magic-only.deb", b"!<arch>\n".to_vec()),
        (
            "noise.deb",
            (0..5000u32).map(|i| (i * 31 % 251) as u8).collect(),
        ),
        ("text.deb", b"Package: sneaky\nVersion: 1\n".to_vec()),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        let error = inspect_package(&path.display().to_string())
            .expect_err("nothing here is a Debian package");
        assert!(
            error.contains("not a readable Debian package"),
            "{name}: {error}"
        );
    }
}

#[test]
fn a_directory_named_like_a_package_is_refused_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake.deb");
    fs::create_dir(&fake).unwrap();
    let error = inspect_package(&fake.display().to_string()).unwrap_err();
    assert!(error.contains("not a regular file"), "{error}");
    assert!(!error.contains("'\""), "no doubled quoting: {error}");
}
