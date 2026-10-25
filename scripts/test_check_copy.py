#!/usr/bin/env python3
"""Unit tests for scripts/check-copy.py.

Run from anywhere: `python3 -I scripts/test_check_copy.py`.
"""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-copy.py"
_spec = importlib.util.spec_from_file_location("check_copy", SCRIPT)
copy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(copy)

EM, EN = "\u2014", "\u2013"


def hits(text: str, rel: str = "docs/page.md", allow=()):
    problems, _ = copy.check([rel], list(allow), read=lambda _: text)
    return problems


class Dashes(unittest.TestCase):
    def test_em_and_en_dashes_are_flagged_everywhere(self):
        for rel in ("docs/page.md", "website/index.html", "site/x.py", ".github/workflows/ci.yml"):
            for text in (f"one {EM} two\n", f"pages 3{EN}5\n", f"`code {EM} too`\n"):
                with self.subTest(rel=rel, text=text):
                    self.assertEqual(len(hits(text, rel)), 1)

    def test_the_message_names_the_dash(self):
        self.assertIn("em dash", hits(f"a {EM} b\n")[0])
        self.assertIn("en dash", hits(f"1{EN}2\n")[0])

    def test_hyphens_and_minus_pass(self):
        self.assertEqual(hits("a well-known 1-2 range, x - y and --flag\n"), [])

    def test_crates_and_binary_files_are_not_scanned_yet(self):
        self.assertEqual(hits(f"a {EM} b\n", "crates/hub/src/x.rs"), [])
        self.assertEqual(hits(f"a {EM} b\n", "website/og.png"), [])


class Phrases(unittest.TestCase):
    def test_stock_phrases_in_prose_are_flagged(self):
        for text in ("A seamless upload.\n", "We leverage chunks.\n", "Robust storage.\n",
                     "Unlock your data.\n", "Delve into Xet.\n", "In today's world.\n",
                     "Blazing fast.\n"):
            with self.subTest(text=text):
                self.assertEqual(len(hits(text)), 1)

    def test_code_is_not_prose(self):
        for text in ("Run `unlock --robust`.\n", "```\nleverage()\n```\n",
                     '<code>seamless</code> and <a class="robust">link</a>\n'):
            with self.subTest(text=text):
                self.assertEqual(hits(text, "website/index.html" if "<" in text else "docs/page.md"), [])

    def test_plain_contrast_passes(self):
        # "not just" is ordinary technical English; the voice rules cover the
        # slogan form, which a pattern can't tell apart.
        self.assertEqual(hits("A URL must outlive the next sleep, not just the base.\n"), [])

    def test_phrases_only_apply_to_markdown_and_html(self):
        self.assertEqual(hits("# robust parser\n", "scripts/x.py"), [])

    def test_the_voice_rules_may_quote_them(self):
        self.assertEqual(hits('Avoid "seamless".\n', "design.md"), [])


class Allowlist(unittest.TestCase):
    def test_an_entry_silences_its_line(self):
        allow = [("docs/page.md", "quoted", 1)]
        problems, used = copy.check(["docs/page.md"], allow, read=lambda _: f"quoted {EM} text\n")
        self.assertEqual((problems, used), ([], {1}))

    def test_an_entry_needs_a_reason(self):
        path = Path(self.id().replace(".", "_") + ".allow")
        try:
            path.write_text("docs/page.md | snippet\n", encoding="utf-8")
            with self.assertRaises(ValueError):
                copy.load_allow(path)
        finally:
            path.unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()
