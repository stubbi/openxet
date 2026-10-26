#!/usr/bin/env python3
"""Checks public copy for em and en dashes and for stock AI phrasing.

Owner ruling (2026-10-25): public copy never uses an em dash (U+2014) or an
en dash (U+2013), and nothing may read as machine-written. design.md
("Voice and vocabulary") states both rules; this enforces the part a machine
can check:
- no U+2013 or U+2014 in any tracked text file in SCAN;
- none of PHRASES in the prose of Markdown and HTML files, outside code.

crates/ joins SCAN with the renaming of user-facing strings (Rust string
literals and doc comments reach docs.rs and the terminal).

A justified exception goes in scripts/check-copy.allow as
`path | snippet | reason`, where the snippet is text on the flagged line. An
entry that matches nothing is itself an error, so the list stays exact.

Run from anywhere: `python3 -I scripts/check-copy.py [PATH...]`. With no
arguments it scans every tracked file SCAN matches. Exits 0 when clean, 1
naming each finding, 2 on a usage or configuration error.
"""

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
ALLOW = REPO / "scripts/check-copy.allow"

TEXT_SUFFIXES = {
    ".md", ".html", ".txt", ".svg", ".json", ".yml", ".yaml", ".toml",
    ".py", ".mjs", ".js", ".ts", ".tsx", ".css", ".sh",
}
# Directories outside the check for now; see the module doc.
SKIP_PREFIXES = ("crates/", "testdata/")
PROSE_SUFFIXES = {".md", ".html"}

DASHES = re.compile("[\u2013\u2014]")
PHRASES = re.compile(
    r"\b(?:seamless(?:ly)?|leverag(?:e|es|ed|ing)|robust(?:ly|ness)?|unlock(?:s|ed|ing)?"
    r"|empower(?:s|ed|ing)?|elevat(?:e|es|ed|ing)|effortless(?:ly)?|cutting-edge"
    r"|game-chang(?:er|ing)|delv(?:e|es|ing)|in today's|best-in-class"
    r"|blazing(?:ly)?|supercharg(?:e|es|ed|ing))\b",
    re.IGNORECASE,
)
# Code spans, fenced code and HTML tags are not prose.
CODE_SPAN = re.compile(r"`[^`\n]*`|<code>.*?</code>|<pre>.*?</pre>|<[^<>\n]+>", re.DOTALL)
FENCE = re.compile(r"^[ \t]*(`{3,}|~{3,})")
# The voice rules quote the phrases they forbid.
PHRASE_EXEMPT = {"design.md", "scripts/check-copy.py", "scripts/test_check_copy.py"}


def scanned(rel: str) -> bool:
    return Path(rel).suffix in TEXT_SUFFIXES and not rel.startswith(SKIP_PREFIXES)


def prose_mask(text: str) -> str:
    """The text with code (fenced, spans, tags) blanked to spaces, so
    offsets and line numbers stay the same."""
    out, fence = [], None
    for line in text.splitlines(keepends=True):
        m = FENCE.match(line)
        if fence is None and m:
            fence = m.group(1)
            out.append(re.sub(r"[^\n]", " ", line))
        elif fence is not None:
            if m and m.group(1)[0] == fence[0] and line.strip() == m.group(1):
                fence = None
            out.append(re.sub(r"[^\n]", " ", line))
        else:
            out.append(line)
    masked = "".join(out)
    return CODE_SPAN.sub(lambda m: re.sub(r"[^\n]", " ", m.group(0)), masked)


def findings(rel: str, text: str):
    """(line number, column, line, message) for each problem in one file."""
    hits = [(m.start(), f"{'em' if m.group(0) == '\u2014' else 'en'} dash: rewrite with "
                       f"a full stop, comma, colon or parentheses (ranges: \"to\")")
            for m in DASHES.finditer(text)]
    if Path(rel).suffix in PROSE_SUFFIXES and rel not in PHRASE_EXEMPT:
        hits += [(m.start(), f"stock phrase {m.group(0)!r}: say the concrete thing instead")
                 for m in PHRASES.finditer(prose_mask(text))]
    for at, message in sorted(hits):
        line_no = text.count("\n", 0, at) + 1
        start = text.rfind("\n", 0, at) + 1
        end = text.find("\n", at)
        yield line_no, at - start + 1, text[start:end if end != -1 else len(text)], message


def load_allow(path: Path):
    entries = []
    if not path.exists():
        return entries
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = [part.strip() for part in line.split("|")]
        if len(parts) != 3 or not all(parts):
            raise ValueError(f"{path.name}:{n}: expected `path | snippet | reason`, got {line!r}")
        entries.append((parts[0], parts[1], n))
    return entries


def check(paths, allow, read=lambda rel: (REPO / rel).read_text(encoding="utf-8")):
    """Returns (problems, used allowlist line numbers)."""
    problems, used = [], set()
    for rel in paths:
        if not scanned(rel):
            continue
        try:
            text = read(rel)
        except UnicodeDecodeError:
            continue
        for line_no, col, line, message in findings(rel, text):
            hit = [n for r, s, n in allow if r == rel and s in line]
            if hit:
                used.update(hit)
                continue
            problems.append(f"{rel}:{line_no}:{col}: {message}\n    {line.strip()}")
    return problems, used


def tracked_paths():
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, check=True,
                         capture_output=True).stdout.decode()
    return [p for p in out.split("\0") if p]


def main(argv):
    if any(a.startswith("-") for a in argv):
        print(__doc__.strip(), file=sys.stderr)
        return 2
    try:
        allow = load_allow(ALLOW)
    except ValueError as err:
        print(f"check-copy: {err}", file=sys.stderr)
        return 2
    problems, used = check(argv or tracked_paths(), allow)
    if not argv:
        problems += [
            f"{ALLOW.relative_to(REPO)}:{n}: entry `{r} | {s}` matches nothing; remove it"
            for r, s, n in allow if n not in used
        ]
    for problem in problems:
        print(problem)
    if problems:
        print(f"check-copy: {len(problems)} problem(s). The rules are in design.md "
              f"(Voice and vocabulary).", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
