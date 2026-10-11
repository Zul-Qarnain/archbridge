//! A finished build must not leave its working tree behind in
//! `.archbridge/jobs/<job>`, unless the user asked to keep it. No toolchain is
//! needed: on this box `makepkg` is absent, so the build fails — which is
//! exactly the exit path that used to leak the directory too.

use archbridge::build::clean_job_dir;
use archbridge::engine::Engine;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fake_job(root: &Path) -> PathBuf {
    let job = root.join(".archbridge").join("jobs").join("job-x");
    fs::create_dir_all(job.join("chroot/root/usr")).unwrap();
    fs::create_dir_all(job.join("src/demo")).unwrap();
    fs::create_dir_all(job.join("pkg")).unwrap();
    fs::write(job.join("PKGBUILD"), "pkgname=demo\n").unwrap();
    fs::write(job.join("build.log"), "==> native makepkg\n").unwrap();
    fs::write(job.join("demo-1.tar.gz"), vec![0u8; 4096]).unwrap();
    fs::write(job.join("demo-1-1-x86_64.pkg.tar.zst"), b"pkg").unwrap();
    job
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn a_finished_job_keeps_only_the_pkgbuild_log_and_the_artifact() {
    let root = TempDir::new().unwrap();
    let job = fake_job(root.path());
    let leftovers = clean_job_dir(&job, true);
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(
        names(&job),
        ["PKGBUILD", "build.log", "demo-1-1-x86_64.pkg.tar.zst"]
    );
}

#[test]
fn a_published_artifact_leaves_no_copy_in_the_job_directory() {
    let root = TempDir::new().unwrap();
    let job = fake_job(root.path());
    clean_job_dir(&job, false);
    assert_eq!(names(&job), ["PKGBUILD", "build.log"]);
}

#[test]
fn cleanup_refuses_a_directory_that_is_not_a_job_directory() {
    let root = TempDir::new().unwrap();
    let project = root.path().join("my-project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("main.rs"), "fn main() {}").unwrap();
    let leftovers = clean_job_dir(&project, false);
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    assert!(project.join("main.rs").exists() && project.join("src").exists());

    // Not even a directory *named* like a job, unless it sits in .archbridge/jobs.
    let lookalike = root.path().join("jobs").join("job-y");
    fs::create_dir_all(&lookalike).unwrap();
    fs::write(lookalike.join("data"), "x").unwrap();
    assert_eq!(clean_job_dir(&lookalike, false).len(), 1);
    assert!(lookalike.join("data").exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_job_is_removed_as_a_link_not_followed() {
    let root = TempDir::new().unwrap();
    let job = fake_job(root.path());
    let precious = root.path().join("precious");
    fs::create_dir_all(&precious).unwrap();
    fs::write(precious.join("keep.txt"), "do not delete").unwrap();
    std::os::unix::fs::symlink(&precious, job.join("src/escape")).unwrap();
    std::os::unix::fs::symlink(&precious, job.join("escape-top")).unwrap();
    clean_job_dir(&job, false);
    assert!(precious.join("keep.txt").exists());
    assert!(!job.join("escape-top").exists());
}

#[test]
fn a_missing_job_directory_is_not_an_error() {
    let root = TempDir::new().unwrap();
    let job = root.path().join(".archbridge/jobs/never-created");
    assert!(clean_job_dir(&job, true).is_empty());
}

/// Drive a real (failing) build through the engine in the current directory.
fn failed_build(keep_jobs: bool) -> PathBuf {
    let temp = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.16)",
    )
    .unwrap();
    let mut engine = Engine::with_config_path(&temp.path().join("config.json"));
    engine.config.keep_jobs = keep_jobs;
    let plan = engine
        .prepare_build(project.path().to_str().unwrap(), None, None, None, &[], &[])
        .expect("a cmake directory stages a build");
    let job = PathBuf::from(&plan.filesystem_changes[0].path);
    assert!(job.starts_with(".archbridge/jobs"), "{job:?}");
    // makepkg/mkarchroot do not exist here, so the build ends in an error.
    let _ = engine.execute_plan(&plan.plan_id, true);
    job
}

#[test]
fn a_failed_build_removes_its_working_tree_but_keeps_the_log() {
    let job = failed_build(false);
    assert!(
        job.join("build.log").exists(),
        "the failure must stay explainable"
    );
    let leftover: Vec<String> = names(&job)
        .into_iter()
        .filter(|n| n != "PKGBUILD" && n != "build.log")
        .collect();
    assert!(leftover.is_empty(), "left behind: {leftover:?}");
    let _ = fs::remove_dir_all(&job);
}

#[test]
fn keep_jobs_preserves_the_working_tree_on_request() {
    let job = failed_build(true);
    let all = names(&job);
    assert!(
        all.iter().any(|n| n.ends_with(".tar.gz") || n == "chroot"),
        "keep_jobs must leave the sources/chroot alone: {all:?}"
    );
    let _ = fs::remove_dir_all(&job);
}
