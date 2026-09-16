use archbridge::config::Config;
use archbridge::engine::Engine;
use archbridge::recipe::{detect_build_system, generate_pkgbuild, BuildSystem, PkgbuildParams};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_config_defaults_and_toggle() {
    let mut config = Config::default();
    assert!(config.official);
    assert!(config.aur);

    assert!(config.set("aur", "false").is_ok());
    assert!(!config.aur);
    assert_eq!(config.is_enabled("aur"), false);

    assert!(config
        .set("repo.mytool", "https://github.com/owner/repo")
        .is_ok());
    assert_eq!(
        config.get("repo.mytool").unwrap(),
        serde_json::Value::String("https://github.com/owner/repo".to_string())
    );
}

#[test]
fn test_recipe_cmake_detection() {
    let temp = TempDir::new().unwrap();
    let cmake_file = temp.path().join("CMakeLists.txt");
    fs::write(&cmake_file, "cmake_minimum_required(VERSION 3.10)").unwrap();

    let sys = detect_build_system(temp.path());
    assert_eq!(sys, BuildSystem::Cmake);

    let params = PkgbuildParams {
        name: "testpkg".to_string(),
        version: "1.0.0".to_string(),
        source_tarball: "src.tar.gz".to_string(),
        sha256_hash: "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff".to_string(),
        dependencies: vec!["glibc".to_string()],
        entry_binary: Some("testpkg".to_string()),
    };

    let pkgbuild = generate_pkgbuild(&sys, &params).unwrap();
    assert!(pkgbuild.contains("pkgname='testpkg'"));
    assert!(pkgbuild.contains("cmake -B build"));
    assert!(!pkgbuild.contains("SKIP"));
}

#[test]
fn test_recipe_unsupported_detection() {
    let temp = TempDir::new().unwrap();
    let sys = detect_build_system(temp.path());
    match sys {
        BuildSystem::Unsupported(msg) => assert!(msg.contains("unsupported build system")),
        _ => panic!("Expected unsupported build system"),
    }
}

#[test]
fn test_engine_plan_preparation() {
    let mut engine = Engine::new();
    let plan = engine.prepare_search("bash", None).unwrap();
    assert_eq!(plan.action, "search");
    assert!(plan.decision.is_some());
}
