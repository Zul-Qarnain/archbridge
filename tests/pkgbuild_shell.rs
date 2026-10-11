use archbridge::recipe::{generate_pkgbuild, BuildSystem, PkgbuildParams};
use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

fn params(src_subdir: Option<&str>) -> PkgbuildParams {
    PkgbuildParams {
        name: "demo-tool".to_string(),
        version: "1:2.3.4-5".to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "b".repeat(64),
        dependencies: vec!["glibc".to_string(), "openssl".to_string()],
        entry_binary: Some("demo-tool".to_string()),
        src_subdir: src_subdir.map(str::to_string),
        architecture: Some("amd64".to_string()),
    }
}

/// Every template is shell makepkg will execute. A quoting mistake is invisible
/// in a string assertion, so bash has to accept the file before it is staged.
#[test]
fn every_generated_pkgbuild_is_parsable_shell() {
    if Command::new("bash").arg("--version").output().is_err() {
        eprintln!("bash is unavailable; skipping");
        return;
    }

    let systems = vec![
        BuildSystem::Imported,
        BuildSystem::Cmake,
        BuildSystem::Make,
        BuildSystem::Cargo,
        BuildSystem::Go,
        BuildSystem::Npm,
        BuildSystem::Yarn,
        BuildSystem::Meson,
    ];
    let temp = TempDir::new().unwrap();

    for system in systems {
        for subdir in [None, Some("demo-tool-1.2.3")] {
            let script = generate_pkgbuild(&system, &params(subdir)).unwrap();
            let path = temp.path().join(format!("{:?}-{}.PKGBUILD", system, 1));
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(script.as_bytes()).unwrap();
            drop(file);

            let check = Command::new("bash")
                .arg("-n")
                .arg(&path)
                .output()
                .expect("failed to run bash");
            assert!(
                check.status.success(),
                "{:?} PKGBUILD for {:?} is not parsable:\n{}\n{}",
                system,
                subdir,
                script,
                String::from_utf8_lossy(&check.stderr)
            );

            if let (Some(dir), false) = (subdir, matches!(&system, BuildSystem::Imported)) {
                // Imported payloads are repacked without compiling, so they never
                // enter an extracted source tree.
                assert!(script.contains(&format!("cd \"$srcdir/{}\"", dir)));
            }
            assert!(script.contains("pkgver='2.3.4_5'"));
            assert!(script.contains("epoch=1\n"));
            assert!(script.contains("depends=('glibc' 'openssl')"));
        }
    }
}

/// A foreign payload can already ship `/usr/bin/<name>` as a symlink to an
/// absolute target that only resolves once installed. Inside `$pkgdir` that
/// link is broken, so an `[ ! -e ]` guard alone lets the generated `ln -s` run
/// onto the existing path and abort the whole build with "File exists". The
/// real `package()` body has to leave an already-present launcher untouched.
#[test]
fn imported_package_does_not_relink_an_existing_usr_bin_entry() {
    for program in ["bash", "tar"] {
        if Command::new(program).arg("--version").output().is_err() {
            eprintln!("{program} is unavailable; skipping");
            return;
        }
    }

    let temp = TempDir::new().unwrap();
    let payload = temp.path().join("payload");
    let srcdir = temp.path().join("src");
    let pkgdir = temp.path().join("pkg");
    std::fs::create_dir_all(payload.join("opt/qoder")).unwrap();
    std::fs::create_dir_all(payload.join("usr/bin")).unwrap();
    std::fs::create_dir_all(&srcdir).unwrap();
    std::fs::create_dir_all(&pkgdir).unwrap();
    std::fs::write(payload.join("opt/qoder/qoder"), "#!/bin/sh\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            payload.join("opt/qoder/qoder"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    // The launcher ships as an absolute symlink, exactly as a .deb would.
    Command::new("ln")
        .args(["-s", "/opt/qoder/qoder"])
        .arg(payload.join("usr/bin/qoder"))
        .status()
        .expect("failed to create the payload symlink");
    let archive = temp.path().join("src.tar.gz");
    Command::new("tar")
        .args(["-czf", archive.to_str().unwrap(), "."])
        .current_dir(&payload)
        .status()
        .expect("failed to create the source tarball");
    std::fs::copy(&archive, srcdir.join("src.tar.gz")).unwrap();

    let script = generate_pkgbuild(
        &BuildSystem::Imported,
        &PkgbuildParams {
            name: "qoder".to_string(),
            version: "1:0.4.2-1".to_string(),
            source_tarball: "src.tar.gz".to_string(),
            sha256_hash: "b".repeat(64),
            dependencies: vec![],
            entry_binary: Some("qoder".to_string()),
            src_subdir: None,
            architecture: Some("amd64".to_string()),
        },
    )
    .unwrap();
    let pkgbuild = temp.path().join("PKGBUILD");
    std::fs::write(&pkgbuild, &script).unwrap();

    let run = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source '{pkgbuild}'; package",
            pkgbuild = pkgbuild.display()
        ))
        .env("srcdir", &srcdir)
        .env("pkgdir", &pkgdir)
        .output()
        .expect("failed to run package()");

    let stderr = String::from_utf8_lossy(&run.stderr).to_string();
    assert!(
        run.status.success(),
        "package() aborted on an existing launcher:\n{script}\n{stderr}"
    );
    assert!(
        !stderr.contains("File exists"),
        "package() tried to relink an existing path:\n{stderr}"
    );
    assert!(
        pkgdir.join("usr/bin/qoder").symlink_metadata().is_ok(),
        "the shipped launcher must survive package()"
    );
}

/// `makepkg --printsrcinfo` parses the control file the same way a build will,
/// which catches a field makepkg rejects before any chroot is created.
#[test]
fn makepkg_accepts_the_generated_pkgbuilds() {
    let Some(makepkg) = ["makepkg", "/usr/bin/makepkg"]
        .into_iter()
        .find(|program| Command::new(program).arg("--version").output().is_ok())
    else {
        eprintln!("makepkg is unavailable; skipping");
        return;
    };

    let temp = TempDir::new().unwrap();
    for system in [BuildSystem::Cargo, BuildSystem::Go, BuildSystem::Imported] {
        let script = generate_pkgbuild(&system, &params(Some("demo-tool-1.2.3"))).unwrap();
        let dir = temp.path().join(format!("{:?}", system));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("PKGBUILD"), &script).unwrap();
        std::fs::write(dir.join("src.tar.gz"), b"placeholder").unwrap();

        let parsed = Command::new(makepkg)
            .args(["--printsrcinfo"])
            .current_dir(&dir)
            .output()
            .expect("failed to run makepkg");
        assert!(
            parsed.status.success(),
            "makepkg rejected the {:?} PKGBUILD:\n{}\n{}",
            system,
            script,
            String::from_utf8_lossy(&parsed.stderr)
        );
        let text = String::from_utf8_lossy(&parsed.stdout).to_string();
        assert!(text.contains("pkgbase = demo-tool"), "{}", text);
    }
}
