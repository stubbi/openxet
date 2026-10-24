#!/usr/bin/env python3
"""Writes /llms.txt and /sitemap.xml for the built site from docs/SUMMARY.md.

Usage: generate-site-index.py PUBLIC_DIR (run by scripts/build-site.sh after
mdBook has written PUBLIC_DIR/docs).

llms.txt (https://llmstxt.org) is site/llms-template.txt with `{docs}`
replaced by one section per part of the navigation, linking each page's
plain Markdown copy (design §7.1). A page's note is the first sentence of
its opening paragraph, when that paragraph is prose.

sitemap.xml lists the landing page and every docs page.
"""

import re
import sys
from pathlib import Path
from xml.sax.saxutils import escape

REPO = Path(__file__).resolve().parents[1]
SUMMARY = REPO / "docs/SUMMARY.md"
TEMPLATE = REPO / "site/llms-template.txt"
SITE = "https://openxet.com"

PART = re.compile(r"^# (.+)$")
ENTRY = re.compile(r"^\s*(?:- )?\[([^\]]+)\]\(([^)]*)\)\s*$")
LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
SENTENCE = re.compile(r"^(.+?[.!?])(?:\s|$)")
NOT_PROSE = re.compile(r"^(?:[-*+|>#]|\d+\.|```|Date:)")


def navigation() -> list[tuple[str, list[tuple[str, str]]]]:
    """Parts of the navigation, each with its (title, page) entries in order."""
    parts: list[tuple[str, list[tuple[str, str]]]] = []
    for line in SUMMARY.read_text(encoding="utf-8").splitlines():
        if part := PART.match(line):
            if part.group(1) != "Summary":
                parts.append((part.group(1), []))
        elif (entry := ENTRY.match(line)) and entry.group(2):
            if not parts:
                parts.append(("Docs", []))
            parts[-1][1].append((entry.group(1), entry.group(2)))
    # The prefix chapter (README.md) heads the first part.
    if len(parts) > 1 and parts[0][0] == "Docs":
        prefix = parts.pop(0)[1]
        parts[0][1][:0] = prefix
    return parts


def published(page: str) -> str:
    """The page's path under /docs/ without extension (README.md is index)."""
    stem = page[: -len(".md")]
    return "index" if stem == "README" else stem


def note(markdown: Path) -> str:
    lines = markdown.read_text(encoding="utf-8").splitlines()
    paragraph: list[str] = []
    for line in lines[1:]:
        if line.strip():
            paragraph.append(line.strip())
        elif paragraph:
            break
    if not paragraph or NOT_PROSE.match(paragraph[0]):
        return ""
    text = LINK.sub(r"\1", " ".join(paragraph))
    sentence = SENTENCE.match(text)
    return sentence.group(1) if sentence else text


def llms_txt(docs: Path, parts) -> str:
    sections = []
    for name, entries in parts:
        lines = [f"## {name}", ""]
        for title, page in entries:
            path = published(page)
            line = f"- [{title}]({SITE}/docs/{path}.md)"
            if summary := note(docs / f"{path}.md"):
                line += f": {summary}"
            lines.append(line)
        sections.append("\n".join(lines) + "\n")
    return TEMPLATE.read_text(encoding="utf-8").replace("{docs}", "\n".join(sections))


def sitemap(parts) -> str:
    urls = [f"{SITE}/", f"{SITE}/docs/"]
    urls += [
        f"{SITE}/docs/{published(page)}.html"
        for _, entries in parts
        for _, page in entries
        if published(page) != "index"
    ]
    body = "".join(f"  <url>\n    <loc>{escape(url)}</loc>\n  </url>\n" for url in urls)
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        f"{body}</urlset>\n"
    )


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: generate-site-index.py PUBLIC_DIR", file=sys.stderr)
        return 2
    public = Path(sys.argv[1])
    parts = navigation()
    (public / "llms.txt").write_text(llms_txt(public / "docs", parts), encoding="utf-8")
    (public / "sitemap.xml").write_text(sitemap(parts), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
