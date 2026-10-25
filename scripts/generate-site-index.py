#!/usr/bin/env python3
"""Finishes the rendered site with metadata, accessibility fixes and indexes.

Usage: generate-site-index.py PUBLIC_DIR (run by scripts/build-site.sh after
mdBook has written PUBLIC_DIR/docs).

llms.txt (https://llmstxt.org) is site/llms-template.txt with `{docs}`
replaced by one section per part of the navigation, linking each page's
plain Markdown copy (design §7.1), with a one-line note for each (see
note()). scripts/build-site.sh fails if any entry has none.

sitemap.xml lists indexable marketing canonicals and every indexable docs page.
HTML is patched at parsed tag boundaries, leaving scripts and examples intact.
"""

import re
import sys
from html import escape as html_escape, unescape as html_unescape
from html.parser import HTMLParser
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
NOT_PROSE = re.compile(r"^(?:[-*+|>#<]|\d+\.|```|~~~|Date:|Status:)")
DESCRIPTION = re.compile(r"^<!-- description: (.+?) -->$", re.MULTILINE)


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
    """The page's `<!-- description: ... -->` line, or else the first sentence
    of its first prose paragraph between the title and the first section,
    past metadata lines such as `Date:` and `- Status:`."""
    text = markdown.read_text(encoding="utf-8")
    if described := DESCRIPTION.search(text):
        return described.group(1).strip()
    lines = text.splitlines()
    titles = [i for i, line in enumerate(lines) if line.startswith("# ")]
    paragraph: list[str] = []
    for line in lines[titles[0] + 1 if titles else 0 :] + [""]:
        if line.startswith("## "):
            break
        if line.strip():
            paragraph.append(line.strip())
            continue
        if paragraph and not NOT_PROSE.match(paragraph[0]):
            text = LINK.sub(r"\1", " ".join(paragraph))
            sentence = SENTENCE.match(text)
            return sentence.group(1) if sentence else text
        paragraph = []
    return ""


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


def tag_html(tag: str, attrs: dict[str, str | None]) -> str:
    values = "".join(
        f" {key}" if value is None else f' {key}="{html_escape(value, quote=True)}"'
        for key, value in attrs.items()
    )
    return f"<{tag}{values}>"


class DocsPage(HTMLParser):
    """Patch the pinned mdBook 0.5.4 DOM without forking its full theme.

    The source template is crates/mdbook-html/front-end/templates/index.hbs
    at https://github.com/rust-lang/mdBook/tree/v0.5.4. Offsets preserve all
    unmodified bytes, including script strings that resemble HTML tags.
    """

    def __init__(self, source: str, metadata: str = "", markdown: str = "", noindex=False, title: str = ""):
        super().__init__(convert_charrefs=False)
        self.source, self.metadata, self.markdown = source, metadata, markdown
        # mdBook titles every page "<chapter> - <book>", so the home page,
        # whose chapter is the book, would repeat it; `title` replaces it.
        self.title, self.title_start = title, None
        self.noindex = noindex
        self.edits: list[tuple[int, int, str]] = []
        # HTMLParser's positions count LF only; splitlines also counts Unicode
        # separators and form feeds, which would move edits into page content.
        self.lines = [0] + [match.end() for match in re.finditer("\n", source)]
        self.in_head = False
        self.in_main = False
        self.menu_heading = False
        self.has_page_tools = False
        self.head_closed = False
        self.heading = ""
        self.tables = []
        self.table_caption = None
        self.list_items = []
        self.feed(source)
        self.close()

    def position(self) -> int:
        line, column = self.getpos()
        return self.lines[line - 1] + column

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        original = self.get_starttag_text()
        position = self.position()
        if tag == "head":
            self.in_head = True
        if tag == "title" and self.in_head and self.title:
            self.title_start = position + len(original)
        if tag == "main":
            self.in_main = True
        if self.in_main and tag == "li":
            self.list_items.append({"text": [], "checkboxes": []})
        if self.in_main and self.list_items:
            if tag in ("p", "br", "ul", "ol"):
                self.list_items[-1]["text"].append(" ")
            # mdBook's task controls have no ID or label; authored controls keep
            # their existing naming, including labels associated with an ID.
            if (tag == "input" and values.get("type", "").lower() == "checkbox"
                    and "disabled" in values and not values.get("id")
                    and not values.get("aria-label") and not values.get("aria-labelledby")):
                self.list_items[-1]["checkboxes"].append((position, original, values))
        if values.get("id") == "openxet-page-tools":
            self.has_page_tools = True
        name = (values.get("name") or "").lower()
        prop = values.get("property") or ""
        if self.in_head and (
            (self.metadata and tag == "meta" and (name in ("description", "twitter:card") or prop.startswith("og:")))
            or (self.metadata and tag == "link" and (values.get("rel") == "canonical" or values.get("type") == "text/markdown"))
            or (self.noindex and tag == "meta" and name == "robots")
        ):
            self.edits.append((position, len(original), ""))
            return

        # These controls must have usable names before any JavaScript runs.
        if tag == "input" and values.get("id") == "mdbook-sidebar-toggle-anchor":
            values["aria-label"] = "Show table of contents"
        if tag == "input" and values.get("id") == "mdbook-searchbar":
            values["aria-label"] = "Search documentation"
            values["aria-describedby"] = "mdbook-searchresults-header"
        if tag == "iframe" and "sidebar-iframe-outer" in (values.get("class") or "").split():
            values["title"] = "Table of contents"
        menu_title = tag == "h1" and "menu-title" in (values.get("class") or "").split()
        if menu_title:
            tag = "div"
            self.menu_heading = True
        if self.in_main and tag in ("h1", "h2", "h3", "h4", "h5", "h6") and values.get("id"):
            self.heading = values["id"]
        if self.in_main and tag == "table":
            self.tables.append([position, original, values, self.heading, False, self.table_caption])
            self.table_caption = None
        if tag == "caption" and self.tables:
            self.tables[-1][4] = True
        replacement = tag_html(tag, values)
        if values != dict(attrs) or menu_title:
            self.edits.append((position, len(original), replacement))

    def handle_endtag(self, tag):
        position = self.position()
        if tag == "title" and self.title_start is not None:
            self.edits.append((self.title_start, position - self.title_start, html_escape(self.title)))
            self.title_start = None
        if self.in_main and tag == "li" and self.list_items:
            item = self.list_items.pop()
            label = " ".join("".join(item["text"]).split())
            if label:
                for start, original, values in item["checkboxes"]:
                    values["aria-label"] = label
                    self.edits.append((start, len(original), tag_html("input", values)))
        if self.in_main and self.list_items and tag in ("p", "ul", "ol"):
            self.list_items[-1]["text"].append(" ")
        if tag == "head":
            addition = self.metadata
            if self.noindex:
                addition += '<meta name="robots" content="noindex">\n'
            self.edits.append((position, 0, addition))
            self.in_head = False
            self.head_closed = True
        if tag == "h1" and self.menu_heading:
            end = self.source.index(">", position) + 1
            self.edits.append((position, end - position, "</div>"))
            self.menu_heading = False
        if tag == "table" and self.tables:
            start, original, values, heading, caption, authored_caption = self.tables.pop()
            if authored_caption:
                text, comment_start, comment_length = authored_caption
                self.edits.append((comment_start, comment_length, ""))
                if not caption:
                    self.edits.append((start + len(original), 0, f"<caption>{html_escape(text)}</caption>"))
                    caption = True
            if heading and not caption and not values.get("aria-label") and not values.get("aria-labelledby"):
                values["aria-labelledby"] = heading
                self.edits.append((start, len(original), tag_html("table", values)))
        if tag == "main":
            if self.markdown and not self.has_page_tools:
                self.edits.append((position, 0,
                    '<p id="openxet-page-tools">'
                    f'<a href="{html_escape(self.markdown, quote=True)}">Read this page as Markdown</a>'
                    ' · <a href="https://github.com/stubbi/openxet/issues/new/choose">Report a docs issue</a></p>\n'))
            self.in_main = False

    def handle_data(self, text):
        if self.in_main and self.list_items:
            self.list_items[-1]["text"].append(text)

    def handle_entityref(self, name):
        self.handle_data(html_unescape(f"&{name};"))

    def handle_charref(self, name):
        self.handle_data(html_unescape(f"&#{name};"))

    def handle_comment(self, text):
        # Markdown keeps this author-supplied title next to its table; HTML gets
        # a native caption rather than repeating a shared section heading.
        if self.in_main and text.strip().startswith("table-caption:"):
            caption = text.strip().removeprefix("table-caption:").strip()
            if caption:
                self.table_caption = (caption, self.position(), len(text) + 7)

    def rendered(self) -> str:
        if not self.head_closed:
            raise ValueError("rendered documentation has no closing head tag; check the mdBook template")
        result = self.source
        for start, length, replacement in sorted(self.edits, reverse=True):
            result = result[:start] + replacement + result[start + length:]
        return result


def finish_docs(docs: Path, parts) -> None:
    for _, entries in parts:
        for title, page in entries:
            path = published(page)
            description = note(docs / f"{path}.md")
            if not description:
                raise ValueError(f"docs/{page} needs a description comment or an opening prose paragraph")
            canonical = f"{SITE}/docs/" + ("" if path == "index" else f"{path}.html")
            markdown = f"{SITE}/docs/{path}.md"
            home_title = title if path == "index" else ""
            metadata = [
                tag_html("meta", {"name": "description", "content": description}),
                tag_html("link", {"rel": "canonical", "href": canonical}),
                tag_html("link", {"rel": "alternate", "type": "text/markdown", "title": "Markdown", "href": markdown}),
            ]
            for name, value in {
                "type": "website", "site_name": "OpenXet", "title": home_title or f"{title} · OpenXet docs",
                "description": description, "url": canonical, "image": f"{SITE}/og.png",
                "image:width": "1200", "image:height": "630", "image:alt": "OpenXet — storage for large files",
            }.items():
                metadata.append(tag_html("meta", {"property": f"og:{name}", "content": value}))
            metadata.append(tag_html("meta", {"name": "twitter:card", "content": "summary_large_image"}))
            html = docs / f"{path}.html"
            html.write_text(DocsPage(html.read_text(encoding="utf-8"), "\n".join(metadata) + "\n", markdown, title=home_title).rendered(), encoding="utf-8")
    missing = docs / "404.html"
    if missing.exists():
        missing.write_text(DocsPage(missing.read_text(encoding="utf-8"), noindex=True).rendered(), encoding="utf-8")


class PageIndex(HTMLParser):
    def __init__(self, source):
        super().__init__()
        self.in_head = False
        self.canonicals = []
        self.noindex = False
        self.feed(source)

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if tag == "head":
            self.in_head = True
        if not self.in_head:
            return
        if tag == "link" and values.get("rel") == "canonical":
            self.canonicals.append(values.get("href"))
        if tag == "meta" and (values.get("name") or "").lower() in ("robots", "googlebot"):
            self.noindex |= "noindex" in re.split(r"[,\s]+", (values.get("content") or "").lower())

    def handle_endtag(self, tag):
        if tag == "head":
            self.in_head = False


def sitemap(public: Path) -> str:
    urls = []
    for html in sorted(public.rglob("*.html")):
        page = PageIndex(html.read_text(encoding="utf-8"))
        if page.noindex:
            continue
        relative = html.relative_to(public).as_posix()
        expected = f"{SITE}/" + (relative[:-10] if html.name == "index.html" else relative)
        if page.canonicals != [expected]:
            raise ValueError(f"{relative} needs exactly one canonical URL: {expected}")
        urls.append(expected)
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
    try:
        finish_docs(public / "docs", parts)
        (public / "llms.txt").write_text(llms_txt(public / "docs", parts), encoding="utf-8")
        (public / "sitemap.xml").write_text(sitemap(public), encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
