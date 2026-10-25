#!/usr/bin/env python3
"""Checks that text naming the project writes it OpenXet.

The brand is OpenXet wherever text names the project, product or service.
Lowercase `openxet` stays only where it is an identifier: the command and
binary, crate and package names, `OPENXET_*` variables, paths, schema ids
(`openxet.error.v1`), the domain `openxet.com` and every URL. This flags a
lowercase `openxet` in prose, outside code, URLs and identifiers. A command
named in prose goes in a code span (`openxet serve`), so it reads as the
command and not as the name.

Each scanned path is read by a scanner for its format:
- `markdown`: prose outside fenced code, inline code, URLs, autolinks, link
  targets, reference definitions, HTML tags and front matter (comment
  text, which holds page descriptions, is prose);
- `text`: the whole file as prose, minus code spans and URLs (user-visible
  page sources and plain-text templates);
- `yaml`: only the values of the keys a person reads (`name`, `label`,
  `description`, `title`, `about`, `placeholder`, `value`, `options` items),
  as in issue templates and workflow names.
A new format adds a scanner to SCANNERS and its globs to SCAN; Rust string
literals and doc comments under crates/ are the next one.

A justified exception goes in scripts/check-brand.allow as
`path | snippet`, where the snippet is text on the flagged line. An entry
that matches nothing is itself an error, so the list stays exact.

Run from anywhere: `python3 -I scripts/check-brand.py [PATH...]`. With no
arguments it scans SCAN below; with paths it scans just those, each by the
scanner its SCAN entry gives. Exits 0 when clean, 1 naming each finding,
2 on a usage or configuration error.
"""

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
ALLOW = REPO / "scripts/check-brand.allow"

# (glob, scanner), first match wins; a path no entry matches is not scanned.
# Globs are relative to the repository root; `**` spans directories.
SCAN = [
    ("design.md", None),      # the site rework applies the brand there
    ("*.md", "markdown"),
    ("docs/**.md", "markdown"),
    (".github/**.md", "markdown"),
    ("hosted/**/README.md", "markdown"),
    ("site/llms-template.txt", "markdown"),
    ("site/book.toml", "text"),
    ("hosted/waitlist/app/**.tsx", "text"),
    ("hosted/waitlist/lib/waitlist-pages.mjs", "text"),
    (".github/ISSUE_TEMPLATE/*.yml", "yaml"),
    (".github/workflows/*.yml", "yaml"),
]

# Hyphenated compounds that are prose ("OpenXet-hosted repos"); any other
# `openxet-…` is a crate, package or directory name (`openxet-cas`).
PROSE_SUFFIXES = {"hosted", "owned", "issued", "signed", "shaped"}

NAME = re.compile(r"(?<![A-Za-z0-9_./@$=:\\-])openxet(?![A-Za-z0-9_])")
CRATE = re.compile(r"-([A-Za-z0-9]+)")

URL = r"(?:[a-z][a-z0-9+.-]*://[^\s<>)`'\"|\]]+)"
EMAIL = r"(?:[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})"
CODE = r"(?:(`+)(?:(?!\n[ \t]*\n)[\s\S])*?(?<!`)\1(?!`))"
MARKDOWN_PROTECTED = re.compile(
    "|".join([
        CODE,
        r"<!--|-->",
        r"</?[A-Za-z][^<>\n]*>",
        r"<[a-z]+:[^<>\s]+>",
        r"\]\([^)\s]*(?:\s+\"[^\"]*\")?\)",
        r"^[ ]{0,3}\[[^\]]+\]:[ \t]+\S+",
        URL,
        EMAIL,
    ]),
    re.MULTILINE,
)
TEXT_PROTECTED = re.compile("|".join([CODE, URL, EMAIL]))
FENCE = re.compile(r"^[ \t]*(`{3,}|~{3,})")
YAML_LINE = re.compile(r"^\s*(?P<item>-\s+)?(?:(?P<key>[\w-]+):(?:\s+|$))?(?P<value>.*)$")
YAML_PROSE_KEYS = {"name", "label", "description", "title", "about", "placeholder", "value"}


def is_name(text: str, end: int) -> bool:
    """Whether the `openxet` ending at `end` is the name, not an identifier."""
    rest = text[end:end + 40]
    if re.match(r"\.[A-Za-z0-9]", rest) or rest.startswith(("/", ":/")):
        return False  # openxet.com, openxet.error.v1, openxet/, openxet://
    crate = CRATE.match(rest)
    return crate is None or crate.group(1) in PROSE_SUFFIXES


def prose_hits(text: str, offset: int):
    for m in NAME.finditer(text):
        if is_name(text, m.end()):
            yield offset + m.start()


def scan_spans(text: str, protected: re.Pattern):
    pos = 0
    for m in protected.finditer(text):
        yield from prose_hits(text[pos:m.start()], pos)
        pos = m.end()
    yield from prose_hits(text[pos:], pos)


def scan_markdown(text: str):
    """Offsets of hits in Markdown prose, skipping fenced code."""
    lines = text.splitlines(keepends=True)
    offset, start, fence, chunk_start = 0, 0, None, 0
    if lines and lines[0].rstrip("\n") == "---":   # YAML front matter
        for i in range(1, len(lines)):
            if lines[i].rstrip("\n") in ("---", "..."):
                start = i + 1
                break
    offset = sum(len(line) for line in lines[:start])
    chunk_start = offset
    for line in lines[start:]:
        m = FENCE.match(line)
        if fence is None and m:
            yield from (chunk_start + o for o in scan_spans(text[chunk_start:offset], MARKDOWN_PROTECTED))
            fence = m.group(1)
        elif fence is not None and m and m.group(1)[0] == fence[0] \
                and len(m.group(1)) >= len(fence) and line.strip() == m.group(1):
            fence = None
            chunk_start = offset + len(line)
        offset += len(line)
    if fence is None:
        yield from (chunk_start + o for o in scan_spans(text[chunk_start:], MARKDOWN_PROTECTED))


def scan_text(text: str):
    return scan_spans(text, TEXT_PROTECTED)


def scan_yaml(text: str):
    """Offsets of hits in the values a person reads; list items without a
    key are `options` entries. Block scalars are not followed."""
    offset = 0
    for line in text.splitlines(keepends=True):
        m = YAML_LINE.match(line)
        if m.group("key") in YAML_PROSE_KEYS or (m.group("item") and m.group("key") is None):
            value = line[m.start("value"):]
            yield from (offset + m.start("value") + o for o in scan_text(value))
        offset += len(line)


SCANNERS = {"markdown": scan_markdown, "text": scan_text, "yaml": scan_yaml}


def glob(pattern: str) -> re.Pattern:
    """`**` spans directories, `*` stays within one path segment."""
    parts = (re.escape(p).replace(r"\*", "[^/]*") for p in pattern.split("**"))
    return re.compile(".*".join(parts) + r"\Z")


SCAN_RE = [(glob(pattern), scanner) for pattern, scanner in SCAN]


def scanner_for(rel: str):
    for pattern, scanner in SCAN_RE:
        if pattern.match(rel):
            return scanner
    return None


def tracked_paths():
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, check=True,
                         capture_output=True).stdout.decode()
    return [p for p in out.split("\0") if p]


def load_allow(path: Path):
    entries = []
    if not path.exists():
        return entries
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        rel, sep, snippet = (part.strip() for part in line.partition("|"))
        if not sep or not rel or not snippet:
            raise ValueError(f"{path.name}:{n}: expected `path | snippet`, got {line!r}")
        entries.append((rel, snippet, n))
    return entries


def findings(rel: str, text: str, scanner: str):
    for at in SCANNERS[scanner](text):
        line_no = text.count("\n", 0, at) + 1
        line_start = text.rfind("\n", 0, at) + 1
        line_end = text.find("\n", at)
        line = text[line_start:line_end if line_end != -1 else len(text)]
        yield line_no, at - line_start + 1, line


def check(paths, allow, read=lambda rel: (REPO / rel).read_text(encoding="utf-8")):
    """Returns (problems, used allowlist line numbers)."""
    problems, used = [], set()
    for rel in paths:
        scanner = scanner_for(rel)
        if scanner is None:
            continue
        for line_no, col, line in findings(rel, read(rel), scanner):
            hit = [n for r, s, n in allow if r == rel and s in line]
            if hit:
                used.update(hit)
                continue
            problems.append(
                f"{rel}:{line_no}:{col}: lowercase `openxet` names the project: "
                f"write OpenXet, or put a command, path or identifier in a code span\n"
                f"    {line.strip()}"
            )
    return problems, used


def main(argv):
    if any(a.startswith("-") for a in argv):
        print(__doc__.strip(), file=sys.stderr)
        return 2
    try:
        allow = load_allow(ALLOW)
    except ValueError as err:
        print(f"check-brand: {err}", file=sys.stderr)
        return 2
    paths = argv or tracked_paths()
    problems, used = check(paths, allow)
    if not argv:
        problems += [
            f"{ALLOW.relative_to(REPO)}:{n}: entry `{r} | {s}` matches nothing; remove it"
            for r, s, n in allow if n not in used
        ]
    for problem in problems:
        print(problem)
    if problems:
        print(f"check-brand: {len(problems)} problem(s). The rule is in AGENTS.md (Brand).",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
