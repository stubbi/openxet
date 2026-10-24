#!/usr/bin/env python3
"""Tests for site/repo_links.py: a docs page can include files from the
repository, and nothing outside it. Run by scripts/build-site.sh before the
build; `python3 -I site/test_repo_links.py` runs it alone."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SITE = Path(__file__).resolve().parent
REPO = SITE.parent


def run(content: str, source_path: str = "probe.md") -> subprocess.CompletedProcess:
    chapter = {
        "name": "probe",
        "content": content,
        "number": None,
        "sub_items": [],
        "path": source_path,
        "source_path": source_path,
        "parent_names": [],
    }
    book = {"items": [{"Chapter": chapter}]}
    return subprocess.run(
        [sys.executable, "-I", str(SITE / "repo_links.py")],
        input=json.dumps([{}, book]),
        capture_output=True,
        text=True,
        check=False,
    )


class Includes(unittest.TestCase):
    def assert_refused(self, content: str) -> None:
        result = run(content)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("docs/probe.md", result.stderr)
        self.assertIn("outside the repository", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_include_of_a_repository_file_is_expanded(self):
        result = run("{{#include ../CHANGELOG.md}}", "changelog.md")
        self.assertEqual(result.returncode, 0, result.stderr)
        content = json.loads(result.stdout)["items"][0]["Chapter"]["content"]
        self.assertTrue(content.startswith("# Changelog"))

    def test_relative_path_out_of_the_repository_fails(self):
        self.assert_refused("{{#include ../../../../../../../../etc/hosts}}")

    def test_absolute_path_fails(self):
        self.assert_refused("{{#include /etc/hosts}}")

    def test_anchored_include_in_a_code_block_fails(self):
        # mdBook's links preprocessor expands these itself, in code too.
        self.assert_refused("```\n{{#include ../../../../../../../../etc/hosts:1:3}}\n```")

    def test_other_file_reading_directives_fail(self):
        self.assert_refused("{{#rustdoc_include ../../../../../../../../etc/hosts}}")
        self.assert_refused("{{ #playground /etc/hosts }}")

    def test_symlink_out_of_the_repository_fails(self):
        (REPO / "target").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=REPO / "target") as scratch:
            link = Path(scratch) / "escape.md"
            link.symlink_to("/etc/hosts")
            relative = link.relative_to(REPO).as_posix()
            self.assert_refused(f"{{{{#include ../{relative}}}}}")


if __name__ == "__main__":
    unittest.main()
