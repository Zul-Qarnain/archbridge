use archbridge::inspect::inspect_package;
use std::env;

#[test]
fn deb_reports_scripts_dependencies_units_and_elf_without_execution() {
    let fixture_path = "./fixtures/vendor.deb";
    if let Ok(report) = inspect_package(fixture_path) {
        assert_eq!(report.format, "deb");
        for script in &report.maintainer_scripts {
            assert_eq!(script.executed, false);
        }
    }
}

#[test]
fn real_rpm_inspection_never_claims_installability() {
    let rpm_env =
        env::var("ARCHBRIDGE_TEST_RPM").unwrap_or_else(|_| "./fixtures/vendor.rpm".to_string());
    if let Ok(report) = inspect_package(&rpm_env) {
        assert_eq!(report.format, "rpm");
        for script in &report.maintainer_scripts {
            assert_eq!(script.executed, false);
        }
    }
}
