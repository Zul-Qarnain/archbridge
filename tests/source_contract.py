"""Supplementary source checks; NOT a substitute for compiling/running Rust."""
import pathlib
import re
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class SourceContract(unittest.TestCase):
    def read(self, name):
        return (ROOT / "src" / name).read_text()

    def test_manifest_and_module_files_exist(self):
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        self.assertEqual(manifest["package"]["name"], "archbridge")
        for module in re.findall(r"pub mod (\w+);", self.read("lib.rs")):
            flat = (ROOT / "src" / f"{module}.rs").is_file()
            directory = (ROOT / "src" / module / "mod.rs").is_file()
            self.assertTrue(flat or directory, f"Module '{module}' missing as .rs file or directory mod")

    def test_cli_is_rpc_client_not_packaging_engine(self):
        source = self.read("main.rs")
        self.assertIn('"v1.prepare"', source)
        self.assertIn('"v1.execute"', source)
        self.assertNotIn("makepkg", source)
        self.assertNotIn("mkarchroot", source)
        self.assertNotIn("PKGBUILD=", source)

    def test_no_foreign_script_executor(self):
        source = self.read("inspect.rs")
        self.assertIn("executed: false", source)
        self.assertNotRegex(source, r'Step::new\("(?:sh|bash|ldd)"')
        self.assertNotIn("Command::new", source)

    def test_native_package_review_uses_pinned_bytes(self):
        self.assertIn("package_files(&bytes)", self.read("build.rs"))
        self.assertIn("Some(bytes.to_vec())", self.read("inspect.rs"))

    def test_runtime_launch_has_hard_isolation_flags(self):
        source = self.read("build.rs")
        self.assertIn("--private-users=pick", source)
        self.assertIn("--private-network", source)
        self.assertIn('"65534".to_string()', source)
        self.assertIn("smoke.timeout_seconds = 30", source)
        self.assertNotIn('"--bind="', source)

    def test_no_unsigned_integrity_bypass(self):
        sources = "\n".join(path.read_text() for path in (ROOT / "src").glob("*.rs"))
        for flag in ["--skipinteg", "--skippgpcheck", "--nodeps", "--overwrite"]:
            self.assertNotIn(flag, sources)
        self.assertNotIn("SigLevel = Never", sources)

    def test_dry_run_stops_before_rpc_execution(self):
        source = self.read("main.rs")
        self.assertLess(source.index("if dry_run { return Ok(0); }"), source.index('client.call("v1.execute"'))

    def test_no_phase_two_history_collection(self):
        sources = "\n".join(path.read_text() for path in (ROOT / "src").glob("*.rs"))
        self.assertNotIn("history.json", sources)
        self.assertNotIn("compatibility_score", sources)


if __name__ == "__main__":
    unittest.main()
