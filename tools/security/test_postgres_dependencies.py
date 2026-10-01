"""Guard the PostgreSQL-only project dependency boundary, including the lockfile."""
import tomllib
import unittest
from pathlib import Path


class PostgresDependencyTests(unittest.TestCase):
    def test_only_postgres_driver_is_resolved_and_rsa_is_absent(self):
        root = Path(__file__).resolve().parents[2]
        with (root / "Cargo.lock").open("rb") as handle:
            lock = tomllib.load(handle)
        packages = {package["name"] for package in lock["package"]}
        self.assertIn("sqlx-postgres", packages)
        self.assertFalse(packages & {"sqlx-mysql", "sqlx-sqlite", "rsa"},
                         "Do not restore unused drivers or RSA through umbrella features")


if __name__ == "__main__":
    unittest.main()
