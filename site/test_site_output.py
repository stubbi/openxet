#!/usr/bin/env python3
"""Exercise the site generator against a small rendered mdBook tree."""

import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from html.parser import HTMLParser
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The controls reproduce mdBook 0.5.4's generated index.hbs markup.
DOC_HTML = '''<!DOCTYPE HTML>
<html lang="en"><head><title>Reference - OpenXet docs</title>
<meta name="description" content="Generic book description">
<script>const sample = '<meta name="description" content="leave this alone">';</script>
</head><body>
<input type="checkbox" id="mdbook-sidebar-toggle-anchor" class="hidden">
<label for="mdbook-sidebar-toggle-anchor" aria-label="Toggle Table of Contents"></label>
<nav id="mdbook-sidebar"><noscript><iframe class="sidebar-iframe-outer" src="../toc.html"></iframe></noscript></nav>
<h1 class="menu-title">OpenXet docs</h1>
<input type="search" id="mdbook-searchbar" name="searchbar" aria-controls="mdbook-searchresults-outer" aria-describedby="searchresults-header">
<div id="mdbook-searchresults-outer"><div id="mdbook-searchresults-header"></div></div>
<main><h1 id="commands">Commands</h1><p>Use <code>--json</code>.</p>
<table><tr><th>Command</th><th>Purpose</th></tr><tr><td>version</td><td>Show the version</td></tr></table>
<h2 id="errors">Errors</h2><table aria-label="Exit codes"><tr><th>Code</th></tr></table>
<table><caption>HTTP responses</caption><tr><th>Status</th></tr></table>
</main></body></html>'''


class Document(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.elements = []
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        self.elements.append((tag, dict(attrs)))

    def tags(self, tag, **attrs):
        return [a for t, a in self.elements if t == tag and all(a.get(k) == v for k, v in attrs.items())]

    def by_id(self, id_):
        return next(a for _, a in self.elements if a.get("id") == id_)


class SiteOutput(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.public = self.root / "public"
        for directory in ("scripts", "site", "docs", "public/docs/reference", "public/waitlist"):
            (self.root / directory).mkdir(parents=True, exist_ok=True)
        shutil.copy(REPO / "scripts/generate-site-index.py", self.root / "scripts")
        shutil.copy(REPO / "site/llms-template.txt", self.root / "site")
        (self.root / "docs/SUMMARY.md").write_text(
            "# Summary\n\n[OpenXet docs](README.md)\n\n# User\n\n- [Commands](reference/cli.md)\n",
            encoding="utf-8",
        )
        self.write("docs/index.html", DOC_HTML)
        self.write("docs/index.md", "# OpenXet docs\n\nStart a storage server and transfer a file.\n")
        self.write("docs/reference/cli.html", DOC_HTML)
        self.write("docs/reference/cli.md", '# Commands\n\n<!-- description: Use "--json" & inspect <errors> safely. -->\n')
        self.write("docs/toc.html", '<html><head><meta name="robots" content="noindex"></head><body>Contents</body></html>')
        self.write("docs/404.html", DOC_HTML)
        self.write("index.html", '<html><head><link rel="canonical" href="https://openxet.com/"></head><body>Home</body></html>')
        self.write("waitlist/index.html", '<html><head><link rel="canonical" href="https://openxet.com/waitlist/"></head><body>Join</body></html>')
        self.write("waitlist/received.html", '<html><head><meta name="robots" content="noindex, follow"><link rel="canonical" href="https://openxet.com/waitlist/received.html"></head><body>Received</body></html>')
        self.write("404.html", '<html><head><meta name="robots" content="noindex"></head><body>Missing</body></html>')

    def write(self, relative, text):
        (self.public / relative).write_text(text, encoding="utf-8")

    def generate(self):
        return subprocess.run(
            [sys.executable, "-I", str(self.root / "scripts/generate-site-index.py"), str(self.public)],
            capture_output=True, text=True, check=False,
        )

    def generated(self, relative):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        return Document((self.public / relative).read_text(encoding="utf-8"))

    def test_nested_page_metadata_escapes_text_and_keeps_executable_examples_intact(self):
        doc = self.generated("docs/reference/cli.html")
        self.assertEqual(doc.tags("meta", name="description"), [{"name": "description", "content": 'Use "--json" & inspect <errors> safely.'}])
        self.assertEqual(doc.tags("link", rel="canonical")[0]["href"], "https://openxet.com/docs/reference/cli.html")
        self.assertEqual(doc.tags("meta", property="og:url")[0]["content"], "https://openxet.com/docs/reference/cli.html")
        self.assertEqual(doc.tags("meta", property="og:description")[0]["content"], 'Use "--json" & inspect <errors> safely.')
        self.assertEqual(doc.tags("link", rel="alternate", type="text/markdown")[0]["href"], "https://openxet.com/docs/reference/cli.md")
        html = (self.public / "docs/reference/cli.html").read_text(encoding="utf-8")
        self.assertIn("const sample = '<meta name=\"description\" content=\"leave this alone\">';", html)
        self.assertIn('<p>Use <code>--json</code>.</p>', html)
        self.assertEqual(len(doc.tags("errors")), 0)

    def test_sitemap_includes_marketing_canonicals_but_not_noindex_or_duplicate_index_urls(self):
        doc = self.generated("docs/index.html")
        self.assertEqual(doc.tags("link", rel="canonical"), [{"rel": "canonical", "href": "https://openxet.com/docs/"}])
        urls = ET.parse(self.public / "sitemap.xml").findall("{*}url/{*}loc")
        self.assertEqual(sorted(url.text for url in urls), [
            "https://openxet.com/", "https://openxet.com/docs/",
            "https://openxet.com/docs/reference/cli.html", "https://openxet.com/waitlist/",
        ])

    def test_navigation_and_search_names_and_descriptions_resolve_without_javascript(self):
        doc = self.generated("docs/reference/cli.html")
        self.assertTrue(doc.by_id("mdbook-sidebar-toggle-anchor").get("aria-label"))
        self.assertTrue(doc.by_id("mdbook-searchbar").get("aria-label"))
        target = doc.by_id("mdbook-searchbar")["aria-describedby"]
        self.assertEqual(doc.by_id(target)["id"], "mdbook-searchresults-header")
        self.assertTrue(doc.tags("iframe")[0].get("title"))
        self.assertEqual([a.get("id") for a in doc.tags("h1")], ["commands"])

    def test_tables_use_contextual_headings_without_overwriting_an_explicit_name(self):
        doc = self.generated("docs/reference/cli.html")
        table, named_table, captioned_table = doc.tags("table")
        self.assertEqual(table.get("aria-labelledby"), "commands")
        self.assertEqual(named_table.get("aria-label"), "Exit codes")
        self.assertNotIn("aria-labelledby", named_table)
        self.assertNotIn("aria-labelledby", captioned_table)

    def test_mdbook_task_checkboxes_are_named_from_their_item_text(self):
        # Actual mdBook 0.5.4 output from ADR 0004, including inline code.
        tasks = '''<ul><li>Each rule below must be covered by at least one test:
<ul>
<li><input disabled="" type="checkbox"> the v2 command allowlist: only <code>ls-refs</code> and <code>fetch</code></li>
<li><input disabled="" type="checkbox"> <code>reindex</code> rebuilds refs, <code>ref_log</code> and the pack list from the
bucket, and lists unplaced commits as recoverable</li>
</ul></li></ul>'''
        self.write("docs/reference/cli.html", DOC_HTML.replace("</main>", tasks + "</main>"))
        doc = self.generated("docs/reference/cli.html")
        controls = [item for item in doc.tags("input", type="checkbox") if "disabled" in item]
        self.assertEqual([item.get("aria-label") for item in controls], [
            "the v2 command allowlist: only ls-refs and fetch",
            "reindex rebuilds refs, ref_log and the pack list from the bucket, and lists unplaced commits as recoverable",
        ])
        html = (self.public / "docs/reference/cli.html").read_text(encoding="utf-8")
        self.assertIn("only <code>ls-refs</code> and <code>fetch</code></li>", html)

    def test_task_names_exclude_nested_items_decode_entities_and_preserve_explicit_names(self):
        tasks = '''<ul>
<li><input disabled="" type="checkbox" checked=""> Review <a href="/docs/"><code>--dry-run</code></a> &amp; &quot;confirm&quot;:
<ul><li><input disabled="" type="checkbox"> Keep &#60;files&#62;.</li></ul>
Then stop.</li>
<li><input disabled="" type="checkbox" aria-label="Existing task name"> Other text.</li>
</ul>'''
        self.write("docs/reference/cli.html", DOC_HTML.replace("</main>", tasks + "</main>"))
        self.generated("docs/reference/cli.html")
        doc = self.generated("docs/reference/cli.html")
        controls = [item for item in doc.tags("input", type="checkbox") if "disabled" in item]
        self.assertEqual([item.get("aria-label") for item in controls], [
            'Review --dry-run & "confirm": Then stop.', "Keep <files>.", "Existing task name",
        ])
        self.assertIn("checked", controls[0])
        html = (self.public / "docs/reference/cli.html").read_text(encoding="utf-8")
        self.assertIn('<a href="/docs/"><code>--dry-run</code></a> &amp; &quot;confirm&quot;', html)
        self.assertIn('<ul><li><input disabled="" type="checkbox" aria-label="Keep &lt;files&gt;.">', html)

    def test_authored_table_captions_distinguish_tables_under_one_heading(self):
        tables = '''<!-- table-caption: Process exit codes -->
<div class="table-wrapper"><table><thead><tr><th>Exit</th></tr></thead><tbody><tr><td>0</td></tr></tbody></table></div>
<!-- table-caption: JSON & error codes -->
<div class="table-wrapper"><table><thead><tr><th>Code</th></tr></thead><tbody><tr><td><code>usage</code></td></tr></tbody></table></div>'''
        self.write("docs/reference/cli.html", DOC_HTML.replace("</main>", tables + "</main>"))
        self.generated("docs/reference/cli.html")
        doc = self.generated("docs/reference/cli.html")
        self.assertEqual(len(doc.tags("caption")), 3)
        for table in doc.tags("table")[-2:]:
            self.assertNotIn("aria-labelledby", table)
        html = (self.public / "docs/reference/cli.html").read_text(encoding="utf-8")
        self.assertIn("<caption>Process exit codes</caption>", html)
        self.assertIn("<caption>JSON &amp; error codes</caption>", html)
        self.assertIn("<td><code>usage</code></td>", html)
        self.assertNotIn("<!-- table-caption:", html)

    def test_rerun_does_not_duplicate_metadata_or_markdown_links(self):
        self.generated("docs/reference/cli.html")
        doc = self.generated("docs/reference/cli.html")
        self.assertEqual(len(doc.tags("link", rel="canonical")), 1)
        self.assertEqual(len(doc.tags("meta", name="description")), 1)
        self.assertEqual(len(doc.tags("meta", property="og:title")), 1)
        self.assertEqual(len(doc.tags("a", href="https://openxet.com/docs/reference/cli.md")), 1)

    def test_non_newline_separators_do_not_shift_html_patch_offsets(self):
        for separator in ("\u2028", "\u2029", "\u0085", "\f", "\v"):
            with self.subTest(separator=repr(separator)):
                example = f"<pre><code>before{separator}after</code></pre>\n"
                self.write("docs/reference/cli.html", DOC_HTML.replace('<input type="checkbox"', example + '<input type="checkbox"'))
                doc = self.generated("docs/reference/cli.html")
                self.assertTrue(doc.by_id("mdbook-sidebar-toggle-anchor").get("aria-label"))
                self.assertTrue(doc.by_id("mdbook-searchbar").get("aria-label"))
                html = (self.public / "docs/reference/cli.html").read_text(encoding="utf-8")
                self.assertIn(example, html)
                self.assertIn('<p>Use <code>--json</code>.</p>', html)

    def test_docs_not_found_page_cannot_be_indexed(self):
        doc = self.generated("docs/404.html")
        self.assertEqual(doc.tags("meta", name="robots"), [{"name": "robots", "content": "noindex"}])

    def test_missing_description_fails_with_the_page_to_fix(self):
        self.write("docs/reference/cli.md", "# Commands\n\n## Usage\n")
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("reference/cli.md", result.stderr)
        self.assertIn("description", result.stderr)

    def test_indexable_marketing_page_without_canonical_fails_instead_of_disappearing(self):
        self.write("waitlist/index.html", "<html><head><title>Join</title></head><body>Join</body></html>")
        result = self.generate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("waitlist/index.html", result.stderr)
        self.assertIn("canonical", result.stderr)

    def test_index_suffix_in_a_filename_is_not_treated_as_a_directory_index(self):
        self.write("file-index.html", '<html><head><link rel="canonical" href="https://openxet.com/file-index.html"></head><body>Files</body></html>')
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        urls = ET.parse(self.public / "sitemap.xml").findall("{*}url/{*}loc")
        self.assertIn("https://openxet.com/file-index.html", [url.text for url in urls])

    def test_sitemap_rejects_a_canonical_for_another_origin_or_page(self):
        for canonical in ("https://example.com/waitlist/", "https://openxet.com/"):
            with self.subTest(canonical=canonical):
                self.write("waitlist/index.html", f'<html><head><link rel="canonical" href="{canonical}"></head><body>Join</body></html>')
                result = self.generate()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("waitlist/index.html", result.stderr)


if __name__ == "__main__":
    unittest.main()
