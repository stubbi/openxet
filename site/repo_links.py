#!/usr/bin/env python3
"""mdBook preprocessor: makes the repository's relative links work on the site.

The docs are written to be read on GitHub, so their relative links are
relative to each file's place in the repository. On the site that holds for
links between pages in the book, but not for links to files outside it
(`../../CHANGELOG.md`, a source file, an implementation plan) or for links in
a file included from elsewhere (docs/changelog.md includes CHANGELOG.md,
whose links start at the repository root).

Each relative link is resolved against the file it was written in. A target
that is a page of the book becomes a link to that page; any other file in the
repository becomes a link to it on GitHub; a target that does not exist fails
the build, naming the page and the link. Whole-file `{{#include FILE}}` lines
are expanded here, before mdBook's `links` preprocessor, so the included
file's links resolve against the included file, and a link to that file
leads to the page that includes it.

Protocol: https://rust-lang.github.io/mdBook/for_developers/preprocessors.html
"""

import json
import posixpath
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DOCS = "docs"
GITHUB = "https://github.com/stubbi/openxet"

INCLUDE = re.compile(r"^\{\{#include ([^}:\s]+)\}\}\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"(`+)(?:.+?)\1")
INLINE_LINK = re.compile(r"(\]\()(<[^>]*>|[^)\s]+)")
REFERENCE_DEF = re.compile(r"^(\s{0,3}\[[^\]]+\]:\s+)(\S+)")
EXTERNAL = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|#|/)", re.IGNORECASE)


def chapters(items):
    for item in items:
        chapter = item.get("Chapter") if isinstance(item, dict) else None
        if chapter is None:
            continue
        yield chapter
        yield from chapters(chapter.get("sub_items", []))


def resolve(link: str, written_in: str) -> str:
    return posixpath.normpath(posixpath.join(posixpath.dirname(written_in), link))


def included(content: str, written_in: str) -> list[str]:
    return [resolve(m.group(1), written_in) for m in map(INCLUDE.match, content.split("\n")) if m]


class Rewriter:
    def __init__(self, pages: dict[str, str]):
        # Repository path of each page's text -> its source path in the book.
        self.pages = pages
        self.errors: list[str] = []

    def target(self, link: str, written_in: str, page: str) -> str:
        bare = link[1:-1] if link.startswith("<") else link
        if not bare or EXTERNAL.match(bare):
            return link
        path, hash_, fragment = bare.partition("#")
        path, mark, query = path.partition("?")
        resolved = resolve(path, written_in)
        if resolved.startswith("../") or not (REPO / resolved).exists():
            self.errors.append(f"{written_in}: link `{bare}` points to `{resolved}`, which does not exist")
            return link
        if resolved in self.pages:
            book_path = self.pages[resolved]
            new = posixpath.relpath(book_path, posixpath.dirname(page))
        elif resolved.startswith(DOCS + "/") and not resolved.endswith(".md") and (REPO / resolved).is_file():
            # mdBook copies every non-Markdown file under docs/ into the site.
            new = posixpath.relpath(resolved[len(DOCS) + 1 :], posixpath.dirname(page))
        else:
            kind = "tree" if (REPO / resolved).is_dir() else "blob"
            new = f"{GITHUB}/{kind}/main/{resolved}"
        return new + mark + query + hash_ + fragment

    def rewrite(self, content: str, written_in: str, page: str) -> str:
        out, fence = [], None
        for line in content.split("\n"):
            opened = FENCE.match(line)
            if fence:
                if opened and opened.group(1) == fence:
                    fence = None
                out.append(line)
                continue
            if opened:
                fence = opened.group(1)
                out.append(line)
                continue
            if include := INCLUDE.match(line):
                source = resolve(include.group(1), written_in)
                if not (REPO / source).is_file():
                    self.errors.append(f"{written_in}: `{line.strip()}` names `{source}`, which does not exist")
                    out.append(line)
                    continue
                text = (REPO / source).read_text(encoding="utf-8").rstrip("\n")
                out.append(self.rewrite(text, source, page))
                continue
            out.append(self.rewrite_line(line, written_in, page))
        return "\n".join(out)

    def rewrite_line(self, line: str, written_in: str, page: str) -> str:
        sub = lambda m: m.group(1) + self.target(m.group(2), written_in, page)  # noqa: E731
        if REFERENCE_DEF.match(line):
            return REFERENCE_DEF.sub(sub, line)
        parts, last = [], 0
        for code in INLINE_CODE.finditer(line):
            parts.append(INLINE_LINK.sub(sub, line[last : code.start()]))
            parts.append(code.group(0))
            last = code.end()
        parts.append(INLINE_LINK.sub(sub, line[last:]))
        return "".join(parts)


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "supports":
        return 0
    _context, book = json.load(sys.stdin)
    items = book.get("items", book.get("sections", []))
    all_chapters = [c for c in chapters(items) if c.get("source_path")]
    pages: dict[str, str] = {}
    for chapter in all_chapters:
        written_in = posixpath.join(DOCS, chapter["source_path"])
        for source in included(chapter["content"], written_in):
            pages.setdefault(source, chapter["path"])
    pages.update({posixpath.join(DOCS, c["source_path"]): c["path"] for c in all_chapters})
    rewriter = Rewriter(pages)
    for chapter in all_chapters:
        chapter["content"] = rewriter.rewrite(
            chapter["content"], posixpath.join(DOCS, chapter["source_path"]), chapter["path"]
        )
    if rewriter.errors:
        for error in rewriter.errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    json.dump(book, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
