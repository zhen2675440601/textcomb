#!/usr/bin/env python3
"""Record RSA exposure and audit the lockfile independently, failing closed."""

import argparse
import json
import subprocess
import tempfile
import tomllib
from pathlib import Path


AUDIT_CONFIG = """[advisories]
ignore = []
informational_warnings = ["unmaintained", "unsound", "notice"]
[database]
url = "https://github.com/RustSec/advisory-db.git"
fetch = true
stale = false
[output]
format = "json"
quiet = false
show_tree = true
[target]
arch = []
os = []
[yanked]
enabled = true
update_index = true
"""


def validate_audit_report(report):
    settings = report["settings"]
    if set(settings) != {"ignore", "severity", "target_arch", "target_os", "informational_warnings"}:
        raise ValueError("cargo audit report settings are missing or unsupported")
    if (settings["ignore"] != [] or settings["severity"] is not None
            or settings["target_arch"] != [] or settings["target_os"] != []
            or sorted(settings["informational_warnings"]) != ["notice", "unmaintained", "unsound"]):
        raise ValueError("cargo audit report contains advisory or target filters")
    vulnerabilities = report["vulnerabilities"]
    if (vulnerabilities["found"] is not False or type(vulnerabilities["count"]) is not int
            or vulnerabilities["count"] != 0 or vulnerabilities["list"] != []):
        raise ValueError("cargo audit did not confirm an absence of vulnerabilities")


def command(arguments, root):
    return subprocess.run(
        arguments, cwd=root, text=True, encoding="utf-8", capture_output=True, timeout=300,
        check=False,
    )


def diagnose(root, output, run=command):
    evidence = {"schema_version": 1, "status": "error"}
    transcript = []
    try:
        with (root / "Cargo.lock").open("rb") as handle:
            lock = tomllib.load(handle)
        locked = [package for package in lock["package"] if package["name"] == "rsa"]
        evidence["locked_versions"] = sorted({package["version"] for package in locked})

        # Validate resolution independently; weak optional features may still appear here.
        result = run(
            ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
            root,
        )
        transcript.append(f"cargo metadata exit={result.returncode}\n{result.stderr}")
        if result.returncode != 0:
            raise ValueError("cargo metadata failed")
        metadata = json.loads(result.stdout)
        resolved_ids = {node["id"] for node in metadata["resolve"]["nodes"]}
        resolved = [
            package
            for package in metadata["packages"]
            if package["name"] == "rsa" and package["id"] in resolved_ids
        ]
        evidence["resolved_packages"] = [package["id"] for package in resolved]
        evidence["active_packages"] = []
        if any(package["version"] not in evidence["locked_versions"] for package in resolved):
            raise ValueError("resolved RSA package is missing from the locked versions")
        if not locked:
            evidence["status"] = "not_locked"
        else:
            for version in evidence["locked_versions"]:
                result = run(
                    [
                        "cargo", "tree", "--locked", "--workspace", "--all-features",
                        "--target", "all", "--invert", f"rsa@{version}",
                    ],
                    root,
                )
                transcript.append(
                    f"cargo tree rsa@{version} exit={result.returncode}\n"
                    f"{result.stdout}\n{result.stderr}"
                )
                if result.returncode != 0:
                    raise ValueError("cargo tree failed")
                if result.stdout.strip():
                    matching = [
                        package["id"] for package in resolved if package["version"] == version
                    ]
                    if not matching:
                        raise ValueError("RSA dependency paths have no matching resolved package")
                    evidence["active_packages"].extend(matching)
            evidence["status"] = "active" if evidence["active_packages"] else "locked_inactive"
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        evidence["error"] = str(error)
    (output / "rsa-exposure.json").write_text(
        json.dumps(evidence, indent=2) + "\n", encoding="utf-8"
    )
    (output / "rsa-dependency-paths.txt").write_text("\n".join(transcript), encoding="utf-8")
    return evidence


def audit(root, output, run=command):
    evidence = {"status": "error"}
    stdout = ""
    stderr = ""
    try:
        # cargo-audit gives project config precedence over home config. An isolated
        # working directory with an explicit policy prevents either from filtering.
        (output / "rust-audit-config.toml").write_text(AUDIT_CONFIG, encoding="utf-8")
        with tempfile.TemporaryDirectory(prefix="textcomb-audit-") as temporary:
            scanner_root = Path(temporary)
            (scanner_root / ".cargo").mkdir()
            (scanner_root / ".cargo/audit.toml").write_text(AUDIT_CONFIG, encoding="utf-8")
            result = run(["cargo", "audit", "--json", "--file", str(root / "Cargo.lock")], scanner_root)
        stdout, stderr = result.stdout, result.stderr
        evidence["exit_code"] = result.returncode
        if result.returncode != 0:
            raise ValueError("cargo audit failed; inspect retained scan evidence")
        report = json.loads(stdout)
        validate_audit_report(report)
        evidence["status"] = "passed"
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as error:
        evidence["error"] = str(error)
    (output / "rust-audit.json").write_text(stdout, encoding="utf-8")
    (output / "rust-audit-stderr.txt").write_text(stderr, encoding="utf-8")
    return evidence


def check(root, output, run=command):
    output.mkdir(parents=True, exist_ok=True)
    try:
        exposure = diagnose(root, output, run)
    except OSError as error:
        exposure = {"status": "error", "error": f"could not retain RSA evidence: {error}"}
    # Always attempt the scanner, including after failed metadata/tree diagnostics.
    try:
        scan = audit(root, output, run)
    except OSError as error:
        scan = {"status": "error", "error": f"could not retain audit evidence: {error}"}
    passed = exposure["status"] != "error" and scan["status"] == "passed"
    result = {"schema_version": 1, "passed": passed, "rsa_exposure": exposure, "audit": scan}
    (output / "rust-security-status.json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8"
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, default=Path.cwd())
    arguments = parser.parse_args()
    result = check(arguments.root.resolve(), arguments.output.resolve())
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
