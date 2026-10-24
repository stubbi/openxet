#!/usr/bin/env python3
"""Checks that every page under docs/ is in the site's navigation.

docs/SUMMARY.md is the docs site's navigation, and mdBook publishes only the
pages it lists. A page written under docs/ but left out of it, such as a new
reference page, would be in the repository and missing from
https://openxet.com/docs/ and /llms.txt. This fails naming each such page,
and each listed page that does not exist.

Not published, by decision: the implementation plans in
docs/superpowers/plans/ and the ADR template.

Run from anywhere; exits non-zero naming each problem.
"""

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DOCS = REPO / "docs"
SUMMARY = DOCS / "SUMMARY.md"

NOT_PUBLISHED = ("superpowers/plans/", "adr/0000-template.md")
ENTRY = re.compile(r"\]\(([^)]+\.md)\)")


def main() -> int:
    listed = set(ENTRY.findall(SUMMARY.read_text(encoding="utf-8")))
    pages = {
        path.relative_to(DOCS).as_posix()
        for path in DOCS.rglob("*.md")
        if path != SUMMARY
    }
    problems = [
        f"docs/{page} is not in docs/SUMMARY.md; add it to the navigation so the site publishes it"
        for page in sorted(pages - listed)
        if not page.startswith(NOT_PUBLISHED)
    ]
    problems += [
        f"docs/SUMMARY.md lists {page}, which does not exist under docs/"
        for page in sorted(listed - pages)
    ]
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
