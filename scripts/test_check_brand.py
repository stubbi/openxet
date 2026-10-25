#!/usr/bin/env python3
"""Unit tests for scripts/check-brand.py.

Run from anywhere: `python3 -I scripts/test_check_brand.py`.
"""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-brand.py"
_spec = importlib.util.spec_from_file_location("check_brand", SCRIPT)
brand = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(brand)


def hits(text: str, rel: str = "docs/page.md", allow=()):
    problems, _ = brand.check([rel], list(allow), read=lambda _: text)
    return problems


class Markdown(unittest.TestCase):
    def test_prose_name_is_flagged(self):
        for text in ("openxet is a server.\n", "# openxet docs\n",
                     "Data written by openxet's CAS.\n", "an openxet-hosted repo\n",
                     "<!-- description: openxet serves Xet. -->\n",
                     "[openxet docs](README.md)\n", "**openxet** never owns users.\n"):
            with self.subTest(text=text):
                self.assertEqual(len(hits(text)), 1)

    def test_brand_passes(self):
        self.assertEqual(hits("OpenXet is a server; OpenXet-hosted repos.\n"), [])

    def test_identifiers_pass(self):
        for text in (
            "Run `openxet serve`.\n",
            "```bash\nopenxet serve\ncargo build -p openxet\n```\n",
            "~~~\nopenxet upload x\n~~~\n",
            "See openxet.com and openxet.com/docs.\n",
            "The schema `openxet.error.v1` and openxet.labels.v1.\n",
            "The openxet-cas and openxet-protocol crates.\n",
            "Set OPENXET_TOKEN, then read crates/openxet/src/cli.rs.\n",
            "A fork of ggoggam/openxet.\n",
            "Open https://github.com/stubbi/openxet/issues/1.\n",
            "An openxet:// address.\n",
            "[the repo](https://github.com/stubbi/openxet)\n",
            "[repo]: https://github.com/stubbi/openxet\n",
            '<a href="/openxet">x</a>\n',
            "Mail noreply@openxet.com.\n",
            "`openxet\ntoken mint`\n",
        ):
            with self.subTest(text=text):
                self.assertEqual(hits(text), [])

    def test_text_after_a_fence_is_scanned(self):
        self.assertEqual(len(hits("```\nopenxet\n```\nopenxet is here.\n")), 1)

    def test_front_matter_is_skipped(self):
        self.assertEqual(hits("---\nslug: openxet\n---\nOpenXet.\n"), [])

    def test_location_is_reported(self):
        [problem] = hits("Line one.\nThen openxet.\n")
        self.assertTrue(problem.startswith("docs/page.md:2:6: "), problem)


class OtherFormats(unittest.TestCase):
    def test_text_scanner(self):
        self.assertEqual(len(hits('<a href="https://openxet.com/">openxet</a>', "site/book.toml")), 1)
        self.assertEqual(hits('url = "https://github.com/stubbi/openxet"', "site/book.toml"), [])

    def test_yaml_scans_only_values_people_read(self):
        rel = ".github/ISSUE_TEMPLATE/bug.yml"
        self.assertEqual(len(hits("      label: openxet version\n", rel)), 1)
        self.assertEqual(len(hits("        - hosted openxet\n", rel)), 1)
        self.assertEqual(hits("      description: Output of `openxet version --json`.\n", rel), [])
        self.assertEqual(hits("        - openxet.com\n", rel), [])
        self.assertEqual(hits("      run: cargo build -p openxet\n", ".github/workflows/ci.yml"), [])


class Scope(unittest.TestCase):
    def test_paths(self):
        self.assertEqual(brand.scanner_for("README.md"), "markdown")
        self.assertEqual(brand.scanner_for("docs/adr/0001-x.md"), "markdown")
        self.assertIsNone(brand.scanner_for("design.md"))
        self.assertIsNone(brand.scanner_for("compat/README.md"))
        self.assertIsNone(brand.scanner_for("crates/openxet/src/cli.rs"))
        self.assertIsNone(brand.scanner_for("website/index.html"))


class Allowlist(unittest.TestCase):
    def test_entry_silences_its_line_only(self):
        allow = [("docs/page.md", "quoted openxet", 3)]
        self.assertEqual(hits("A quoted openxet line.\n", allow=allow), [])
        self.assertEqual(len(hits("Another openxet line.\n", allow=allow)), 1)

    def test_malformed_entry_is_an_error(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "allow"
            path.write_text("docs/page.md without a separator\n", encoding="utf-8")
            with self.assertRaises(ValueError):
                brand.load_allow(path)


if __name__ == "__main__":
    unittest.main()
