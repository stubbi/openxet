"""The website quotes measurements only from site/measurements.json.

The compat test named in the fixture asserts the same bytes for the pinned
client, so a changed measurement fails CI until the fixture and every page
that quotes it agree. Run: python3 -I site/test_measurements.py
"""

import json
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
FIXTURE = json.loads((REPO / "site/measurements.json").read_text(encoding="utf-8"))
EDIT = FIXTURE["edit_upload"]
QUOTING = [
    REPO / "website/index.html",
    REPO / "website/404.html",
    REPO / "website/about/index.html",
    REPO / "website/privacy/index.html",
    REPO / "website/waitlist/index.html",
    REPO / "website/assets/social-preview.svg",
    REPO / "site/llms-template.txt",
]
MIB = re.compile(r"\b\d+\.\d+ MiB\b")


class Measurements(unittest.TestCase):
    def test_display_matches_the_bytes(self):
        self.assertEqual(EDIT["display"], f"{EDIT['bytes'] / 2**20:.2f} MiB")

    def test_named_test_exists(self):
        path, name = EDIT["test"].split("::")
        self.assertIn(f"def {name}(", (REPO / path).read_text(encoding="utf-8"))

    def test_pages_quote_only_the_fixture(self):
        for page in QUOTING:
            with self.subTest(page=page.relative_to(REPO)):
                text = page.read_text(encoding="utf-8")
                self.assertEqual(set(MIB.findall(text)) - {EDIT["display"]}, set())

    def test_landing_page_marks_and_names_the_measurement(self):
        html = (REPO / "website/index.html").read_text(encoding="utf-8")
        marked = re.findall(r'data-measure="edit-upload">([^<]+)<', html)
        self.assertGreaterEqual(len(marked), 2)
        self.assertEqual(set(marked), {EDIT["display"]})
        self.assertEqual(re.findall(r'data-measure="edit-upload-bytes">([^<]+)<', html), [f"{EDIT['bytes']:,}"])
        self.assertIn(EDIT["test"].split("::")[1], html)


if __name__ == "__main__":
    unittest.main()
