#!/usr/bin/env python3
"""Exercise the public artifact gate before Pages includes hidden paths."""

import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

SITE = Path(__file__).resolve().parent


class PublicArtifact(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.public = Path(self.scratch.name) / "public"
        self.public.mkdir()
        self.write("index.html", "<h1>openxet</h1>")
        self.write("docs/.nojekyll", "")
        self.write_security(datetime.now(timezone.utc) + timedelta(days=90))

    def write(self, relative, text):
        path = self.public / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def write_security(self, expires):
        self.write(".well-known/security.txt", (
            "Contact: https://github.com/stubbi/openxet/security/advisories/new\n"
            f"Expires: {expires.isoformat()}\n"
            "Canonical: https://openxet.com/.well-known/security.txt\n"
            "Policy: https://github.com/stubbi/openxet/security/policy\n"
            "Preferred-Languages: en\n"
        ))

    def check(self):
        return subprocess.run(
            [sys.executable, "-I", str(SITE / "check_public_artifact.py"), str(self.public)],
            capture_output=True, text=True, check=False,
        )

    def assert_refused(self, expected):
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(expected, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_accepts_only_the_intended_hidden_files(self):
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unexpected_hidden_paths_are_rejected_without_reading_their_contents(self):
        for relative in (".env", "assets/.cache/key", ".well-known/extra.txt", "docs/.well-known/security.txt"):
            with self.subTest(relative=relative):
                self.write(relative, "must-not-be-printed")
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(relative.split("/")[0] if relative.startswith(".") else relative, result.stderr)
                self.assertNotIn("must-not-be-printed", result.stderr + result.stdout)
                (self.public / relative).unlink()

    def test_an_allowed_filename_cannot_be_a_directory(self):
        path = self.public / "docs/.nojekyll"
        path.unlink()
        path.mkdir()
        self.assert_refused("docs/.nojekyll")

    def test_symlinks_are_rejected_including_allowed_names_and_visible_paths(self):
        outside = Path(self.scratch.name) / "private"
        outside.write_text("must-not-be-published", encoding="utf-8")
        for relative in (".well-known/security.txt", "docs/.nojekyll", "assets/link"):
            with self.subTest(relative=relative):
                path = self.public / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                if path.exists():
                    path.unlink()
                path.symlink_to(outside)
                self.assert_refused(relative)
                path.unlink()
                self.write_security(datetime.now(timezone.utc) + timedelta(days=90))
                self.write("docs/.nojekyll", "")

    def test_directory_symlinks_are_not_followed(self):
        path = self.public / "assets"
        path.symlink_to(Path(self.scratch.name), target_is_directory=True)
        self.assert_refused("assets")

    def test_missing_security_file_is_rejected(self):
        (self.public / ".well-known/security.txt").unlink()
        self.assert_refused("security.txt")

    def test_stale_or_overlong_expiry_is_rejected(self):
        for days in (-1, 367):
            with self.subTest(days=days):
                self.write_security(datetime.now(timezone.utc) + timedelta(days=days))
                self.assert_refused("Expires")

    def test_expiry_requires_one_valid_timestamp_with_a_timezone(self):
        path = self.public / ".well-known/security.txt"
        for expires in ("", "Expires: later\n", "Expires: 2027-01-01T00:00:00\n", "Expires: 2027-01-01T00:00:00Z\nExpires: 2027-01-02T00:00:00Z\n"):
            with self.subTest(expires=expires):
                path.write_text("Contact: https://github.com/stubbi/openxet/security/advisories/new\n" + expires, encoding="utf-8")
                self.assert_refused("Expires")

    def test_expiry_field_name_is_case_insensitive(self):
        path = self.public / ".well-known/security.txt"
        path.write_text(path.read_text().replace("Expires:", "expires:"), encoding="utf-8")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_expiry_duplicates_are_rejected_regardless_of_case(self):
        path = self.public / ".well-known/security.txt"
        path.write_text(path.read_text() + "expires: 2020-01-01T00:00:00Z\n", encoding="utf-8")
        self.assert_refused("Expires")

    def test_expiry_offset_minutes_cannot_be_normalized_out_of_range(self):
        path = self.public / ".well-known/security.txt"
        path.write_text(path.read_text().replace("+00:00", "+00:99"), encoding="utf-8")
        self.assert_refused("Expires")

    def test_missing_artifact_directory_is_rejected(self):
        result = subprocess.run(
            [sys.executable, "-I", str(SITE / "check_public_artifact.py"), str(self.public / "missing")],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("directory", result.stderr)


if __name__ == "__main__":
    unittest.main()
