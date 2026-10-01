import hashlib
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from rust_audit import AUDIT_CONFIG, audit, check, command, diagnose


def completed(stdout="", stderr="", exit_code=0):
    return subprocess.CompletedProcess([], exit_code, stdout, stderr)


def clean_audit_report():
    return {
        "settings": {"ignore": [], "severity": None, "target_arch": [], "target_os": [],
                     "informational_warnings": ["unmaintained", "unsound", "notice"]},
        "vulnerabilities": {"found": False, "count": 0, "list": []},
    }


class RustAuditTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "evidence"
        self.output.mkdir()
        self.lock(False)

    def lock(self, rsa):
        text = 'version = 4\n[[package]]\nname = "app"\nversion = "0.1.0"\n'
        if rsa:
            text += '[[package]]\nname = "rsa"\nversion = "0.9.10"\n'
        (self.root / "Cargo.lock").write_text(text, encoding="utf-8")

    def run_checks(self, metadata=None, tree=None, audit=None):
        calls = []
        responses = {
            "metadata": metadata or completed(json.dumps({"packages": [], "resolve": {"nodes": []}})),
            "tree": tree or completed("rsa v0.9.10\n└── app v0.1.0\n"),
            "audit": audit or completed(json.dumps(clean_audit_report())),
        }

        def run(arguments, root):
            if arguments[1] == "audit":
                self.assertNotEqual(root, self.root)
                self.assertEqual((root / ".cargo/audit.toml").read_text(encoding="utf-8"), AUDIT_CONFIG)
                self.assertEqual(arguments[3:], ["--file", str(self.root / "Cargo.lock")])
            else:
                self.assertEqual(root, self.root)
            calls.append(arguments[1])
            response = responses[arguments[1]]
            if isinstance(response, Exception):
                raise response
            return response

        return check(self.root, self.output, run), calls

    def active_metadata(self):
        return completed(json.dumps({
            "packages": [{"name": "rsa", "version": "0.9.10", "id": "rsa-id"}],
            "resolve": {"nodes": [{"id": "rsa-id"}]},
        }))

    def test_no_rsa_still_audits_without_missing_package_tree_command(self):
        result, calls = self.run_checks()
        self.assertTrue(result["passed"])
        self.assertEqual(result["rsa_exposure"]["status"], "not_locked")
        self.assertEqual(calls, ["metadata", "audit"])

    def test_inactive_locked_rsa_does_not_exempt_lockfile_vulnerability(self):
        self.lock(True)
        result, calls = self.run_checks(
            metadata=self.active_metadata(), tree=completed(stderr="warning: nothing to print."),
            audit=completed('{"vulnerabilities":{"found":true}}', "known vulnerability", 1),
        )
        self.assertFalse(result["passed"])
        self.assertEqual(result["rsa_exposure"]["status"], "locked_inactive")
        self.assertEqual(calls, ["metadata", "tree", "audit"])
        self.assertIn("known vulnerability", (self.output / "rust-audit-stderr.txt").read_text(encoding="utf-8"))

    def test_active_rsa_retains_real_dependency_paths(self):
        self.lock(True)
        result, calls = self.run_checks(metadata=self.active_metadata())
        self.assertTrue(result["passed"])
        self.assertEqual(result["rsa_exposure"]["status"], "active")
        self.assertEqual(calls, ["metadata", "tree", "audit"])
        self.assertIn("app v0.1.0", (self.output / "rsa-dependency-paths.txt").read_text(encoding="utf-8"))

    def test_metadata_failures_are_not_hidden_or_allowed_to_skip_audit(self):
        for failure in [
            completed(stderr="registry unavailable", exit_code=101),
            completed("invalid metadata"),
            completed("{}"),
            FileNotFoundError("cargo unavailable"),
            subprocess.TimeoutExpired(["cargo", "metadata"], 300),
        ]:
            with self.subTest(failure=failure):
                result, calls = self.run_checks(metadata=failure)
                self.assertFalse(result["passed"])
                self.assertEqual(result["rsa_exposure"]["status"], "error")
                self.assertEqual(result["audit"]["status"], "passed")
                self.assertEqual(calls, ["metadata", "audit"])

    def test_tree_failures_still_audit_and_fail(self):
        self.lock(True)
        for failure in [
            completed(stderr="ambiguous RSA package", exit_code=101),
            FileNotFoundError("cargo unavailable"),
            subprocess.TimeoutExpired(["cargo", "tree"], 300),
        ]:
            with self.subTest(failure=failure):
                result, calls = self.run_checks(metadata=self.active_metadata(), tree=failure)
                self.assertFalse(result["passed"])
                self.assertEqual(result["audit"]["status"], "passed")
                self.assertEqual(calls, ["metadata", "tree", "audit"])

    def test_bad_lock_still_audits_and_cannot_pass(self):
        (self.root / "Cargo.lock").write_text("invalid lockfile", encoding="utf-8")
        result, calls = self.run_checks()
        self.assertFalse(result["passed"])
        self.assertEqual(calls, ["audit"])

    def test_unretained_diagnostic_evidence_does_not_skip_audit_or_pass(self):
        (self.output / "rsa-exposure.json").mkdir()
        result, calls = self.run_checks()
        self.assertFalse(result["passed"])
        self.assertEqual(calls, ["metadata", "audit"])
        self.assertEqual(result["audit"]["status"], "passed")
        self.assertIn("could not retain RSA evidence", result["rsa_exposure"]["error"])

    def test_metadata_cannot_introduce_an_unlocked_rsa_version(self):
        result, calls = self.run_checks(metadata=self.active_metadata())
        self.assertFalse(result["passed"])
        self.assertEqual(calls, ["metadata", "audit"])

    def test_scanner_failures_and_invalid_success_output_fail_closed(self):
        for failure in [
            completed('{"vulnerabilities":{"found":true}}', exit_code=1),
            completed('{"vulnerabilities":{"found":true}}'),
            completed("not JSON"), completed("{}"),
            FileNotFoundError("scanner unavailable"),
            subprocess.TimeoutExpired(["cargo", "audit"], 300),
        ]:
            with self.subTest(failure=failure):
                result, calls = self.run_checks(audit=failure)
                self.assertFalse(result["passed"])
                self.assertEqual(result["audit"]["status"], "error")
                self.assertEqual(calls, ["metadata", "audit"])
                self.assertFalse(json.loads((self.output / "rust-security-status.json").read_text(encoding="utf-8"))["passed"])

    def test_filtered_incomplete_and_inconsistent_success_reports_fail_closed(self):
        failures = []
        for name, value in [
            ("ignore", ["RUSTSEC-2023-0071"]), ("severity", "critical"),
            ("target_arch", ["x86_64"]), ("target_os", ["linux"]),
            ("informational_warnings", ["unmaintained"]),
        ]:
            report = clean_audit_report()
            report["settings"][name] = value
            failures.append(report)
            report = clean_audit_report()
            del report["settings"][name]
            failures.append(report)
        report = clean_audit_report()
        del report["settings"]
        failures.append(report)
        for field, value in [("count", 1), ("count", False), ("list", [{}])]:
            report = clean_audit_report()
            report["vulnerabilities"][field] = value
            failures.append(report)
        for report in failures:
            with self.subTest(report=report):
                result, calls = self.run_checks(audit=completed(json.dumps(report)))
                self.assertFalse(result["passed"])
                self.assertEqual(calls, ["metadata", "audit"])


@unittest.skipUnless(shutil.which("cargo"), "real Cargo fixture checks require Cargo")
class CargoExposureTests(unittest.TestCase):
    def registry_package(self, registry, name, manifest, dependencies=None, features=None):
        version = "0.9.10" if name == "rsa" else "0.1.0"
        archive = registry / f"{name}-{version}.crate"
        with tarfile.open(archive, "w:gz") as handle:
            for suffix, text in [("Cargo.toml", manifest), ("src/lib.rs", "")]:
                body = text.encode("utf-8")
                entry = tarfile.TarInfo(f"{name}-{version}/{suffix}")
                entry.size = len(body)
                handle.addfile(entry, io.BytesIO(body))
        index = registry / "index" / ("3/r" if name == "rsa" else "ad/ap") / name
        index.parent.mkdir(parents=True, exist_ok=True)
        index.write_text(json.dumps({
            "name": name, "vers": version, "deps": dependencies or [],
            "cksum": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "features": {}, "features2": features or {}, "yanked": False, "v": 2,
        }) + "\n", encoding="utf-8")

    def fixture(self, root, rsa, active):
        (root / "app/src").mkdir(parents=True)
        (root / "app/src/lib.rs").write_text("", encoding="utf-8")
        (root / "Cargo.toml").write_text(
            '[workspace]\nresolver="3"\nmembers=["app"]\n',
            encoding="utf-8",
        )
        manifest = '[package]\nname="app"\nversion="0.1.0"\nedition="2024"\n'
        if rsa:
            # A local registry reproduces SQLx-style weak features without network access.
            registry = root / "registry"
            registry.mkdir()
            (root / ".cargo").mkdir()
            (root / ".cargo/config.toml").write_text(
                '[source.crates-io]\nreplace-with="synthetic"\n[source.synthetic]\n'
                f'local-registry={json.dumps(str(registry))}\n', encoding="utf-8"
            )
            self.registry_package(
                registry, "rsa", '[package]\nname="rsa"\nversion="0.9.10"\nedition="2024"\n'
                '[features]\ndummy=[]\n', features={"dummy": []},
            )
            self.registry_package(
                registry, "adapter",
                '[package]\nname="adapter"\nversion="0.1.0"\nedition="2024"\n'
                '[features]\nmysql=["rsa"]\ntypes=["rsa?/dummy"]\n[dependencies]\nrsa={version="0.9.10",optional=true}\n',
                dependencies=[{
                    "name": "rsa", "req": "^0.9.10", "features": [], "optional": True,
                    "default_features": True, "target": None, "kind": "normal",
                }], features={"mysql": ["rsa"], "types": ["rsa?/dummy"]},
            )
            feature = ',features=["types","mysql"]' if active else ',features=["types"]'
            manifest += f'[dependencies]\nadapter={{version="0.1.0"{feature}}}\n'
        (root / "app/Cargo.toml").write_text(manifest, encoding="utf-8")
        result = command(["cargo", "generate-lockfile", "--offline"], root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_real_cargo_distinguishes_absent_inactive_and_active_packages(self):
        for rsa, active, expected in [
            (False, False, "not_locked"), (True, False, "locked_inactive"), (True, True, "active")
        ]:
            with self.subTest(status=expected), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.fixture(root, rsa, active)
                output = root / "evidence"
                output.mkdir()
                result = diagnose(root, output)
                self.assertEqual(result["status"], expected, result)
                if active:
                    self.assertIn("app", (output / "rsa-dependency-paths.txt").read_text(encoding="utf-8"))


@unittest.skipUnless(shutil.which("cargo-audit"), "real filtering checks require cargo-audit")
class CargoAuditPolicyTests(unittest.TestCase):
    def test_project_and_home_filters_cannot_hide_a_real_advisory(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / "project"
            database = base / "advisories"
            output = base / "evidence"
            cargo_home = base / "cargo-home"
            root.mkdir()
            output.mkdir()
            cargo_home.mkdir()
            (root / ".cargo").mkdir()
            filters = '[advisories]\nignore=["RUSTSEC-2026-9999"]\nseverity_threshold="critical"\n'
            (root / ".cargo/audit.toml").write_text(filters, encoding="utf-8")
            (cargo_home / "audit.toml").write_text(filters, encoding="utf-8")
            directory = database / "crates/synthetic-vulnerable"
            directory.mkdir(parents=True)
            (directory / "RUSTSEC-2026-9999.md").write_text('''```toml
[advisory]
id = "RUSTSEC-2026-9999"
package = "synthetic-vulnerable"
date = "2026-01-01"
url = "https://example.invalid/synthetic-regression"
[versions]
patched = [">=1.0.1"]
```
# Synthetic advisory for scanner policy regression
This generated fixture is not a real vulnerability report.
''', encoding="utf-8")
            for arguments in [
                ["init"], ["config", "user.name", "Synthetic fixture"],
                ["config", "user.email", "synthetic@example.invalid"],
                ["add", "."], ["commit", "-m", "synthetic advisory fixture"],
            ]:
                result = command(["git", *arguments], database)
                self.assertEqual(result.returncode, 0, result.stderr)
            lock = root / "Cargo.lock"
            vulnerable_lock = '''version = 4
[[package]]
name = "synthetic-vulnerable"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0000000000000000000000000000000000000000000000000000000000000000"
'''
            lock.write_text(vulnerable_lock, encoding="utf-8")
            local_options = ["--db", str(database), "--no-fetch", "--stale", "--no-yanked"]
            def local_scanner(arguments, scanner_root):
                # Only the fixture changes the database source; production always
                # fetches the official database and scans all targets/advisories.
                return command([*arguments, *local_options], scanner_root)
            with mock.patch.dict("os.environ", {"CARGO_HOME": str(cargo_home)}):
                filtered = local_scanner(["cargo", "audit", "--json", "--file", str(lock)], root)
                self.assertEqual(filtered.returncode, 0, filtered.stderr)
                filtered_report = json.loads(filtered.stdout)
                self.assertFalse(filtered_report["vulnerabilities"]["found"])
                self.assertEqual(filtered_report["settings"]["ignore"], ["RUSTSEC-2026-9999"])
                result = audit(root, output, local_scanner)
                self.assertEqual(result["status"], "error")
                self.assertNotEqual(result["exit_code"], 0)
                full_report = json.loads((output / "rust-audit.json").read_text(encoding="utf-8"))
                self.assertTrue(full_report["vulnerabilities"]["found"])
                self.assertEqual(full_report["settings"], clean_audit_report()["settings"])
                lock.write_text(vulnerable_lock.replace('version = "1.0.0"', 'version = "1.0.1"'), encoding="utf-8")
                self.assertEqual(audit(root, output, local_scanner)["status"], "passed")
            self.assertEqual((root / ".cargo/audit.toml").read_text(encoding="utf-8"), filters)
            self.assertEqual((cargo_home / "audit.toml").read_text(encoding="utf-8"), filters)


if __name__ == "__main__":
    unittest.main()
