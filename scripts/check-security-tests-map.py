#!/usr/bin/env python3
"""Checks docs/reference/security-tests.md against design §5.9 and the tests
that actually run.

Always:
- every test name in design §5.9 is in the map, and §5.9 links the map;
- every M0 or M1 row names a file, and only rows for M2 or later are pending;
- a file cell is a file, `pending`, or `pending (M<n>, <file>)` for a test a
  plan has placed but not written yet, whose `M<n>` matches the row's
  milestone. Planned files are not checked against the tests that run; the
  task that writes the test drops the `pending`.

With `--nextest FILE` (the output of
`cargo nextest list --workspace --message-format json`): every mapped Rust
test is listed exactly once in the binary built from the file the map names,
and is not ignored. A test that was renamed, commented out or lost its
`#[test]` attribute is not listed, so it fails here. One name may be mapped
to several files when each file holds its own half of the requirement.

With `--pytest FILE` (the output of `pytest --collect-only -q`, run in
`compat/`): every mapped Python test, or test module, collects at least one
test from the file the map names. Python helpers outside a test module are
checked to be defined there.

Run from the repository root; exits non-zero naming each problem.
"""

import argparse
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
MAP = REPO / "docs/reference/security-tests.md"
DESIGN = REPO / "docs/superpowers/specs/2026-10-23-openxet-design.md"

NAME = re.compile(r"^`([a-z0-9_]+)`$")
PATH = re.compile(r"^`([\w./-]+\.(?:rs|py))`$")
MILESTONE = re.compile(r"^M(\d+)")
PLANNED = re.compile(r"^pending \(M(\d+), `([\w./-]+\.(?:rs|py))`\)$")


def rows(text: str) -> list[list[str]]:
    return [
        [cell.strip() for cell in line.strip().strip("|").split("|")]
        for line in text.splitlines()
        if line.startswith("| ") and not line.startswith("| ---")
    ]


def map_rows() -> list[list[str]]:
    """Every row of the map's tables, the header rows left out."""
    return [r for r in rows(MAP.read_text()) if r[0] != "Req"]


def mapped(problems: list[str]) -> list[tuple[str, str]]:
    """Each (test, file) the map names, after the static checks."""
    out = []
    for row in map_rows():
        name, path = NAME.match(row[1]), PATH.match(row[2])
        milestone = MILESTONE.match(row[3])
        if milestone and int(milestone.group(1)) <= 1 and not path:
            problems.append(f"{row[1]} is an {milestone.group(0)} row with no file: {row[2]!r}")
        planned = PLANNED.match(row[2])
        if (row[2] == "pending" or planned) and not (milestone and int(milestone.group(1)) >= 2):
            problems.append(f"{row[1]} is pending but its milestone is {row[3]!r}")
        if planned and milestone and planned.group(1) != milestone.group(1):
            problems.append(f"{row[1]} is planned for M{planned.group(1)} but its milestone is {row[3]!r}")
        if not (path or planned or row[2] == "pending"):
            problems.append(f"{row[1]} has a file cell the check can't read: {row[2]!r}")
        if name and path:
            out.append((name.group(1), path.group(1)))
    return out


def check_design(problems: list[str]) -> None:
    section = DESIGN.read_text().split("### 5.9 Security requirements", 1)[1]
    section = section.split("\n## ", 1)[0]
    named = set()
    for row in rows(section):
        if row[0].isdigit():
            named.update(re.findall(r"`([a-z0-9_]+)`", row[-1]))
    if not named:
        problems.append("no test names found in design §5.9")
    on_map = {m.group(1) for r in map_rows() if (m := re.match(r"^`([a-z0-9_]+)`", r[1]))}
    for name in sorted(named - on_map):
        problems.append(f"design §5.9 names {name}, which the map lacks")
    if "security-tests.md" not in section:
        problems.append("design §5.9 does not link the map")


def check_nextest(listing: Path, tests: list[tuple[str, str]], problems: list[str]) -> None:
    found: dict[str, list[tuple[str, bool]]] = {}
    for suite in json.loads(listing.read_text())["rust-suites"].values():
        crate = Path(suite["cwd"]).resolve().relative_to(REPO)
        if suite["kind"] == "test":
            file = str(crate / "tests" / f"{suite['binary-name']}.rs")
        else:
            file = str(crate / "src")
        for case, info in suite["testcases"].items():
            found.setdefault(case.rsplit("::", 1)[-1], []).append((file, info["ignored"]))
    for name, path in tests:
        if not path.endswith(".rs"):
            continue
        hits = found.get(name, [])
        here = [
            ignored
            for file, ignored in hits
            if path == file or (file.endswith("/src") and path.startswith(file + "/"))
        ]
        if not hits:
            problems.append(f"{name}: nextest doesn't list it")
        elif not here:
            files = ", ".join(sorted({file for file, _ in hits}))
            problems.append(f"{name}: runs from {files}, but the map says {path}")
        elif len(here) != 1:
            problems.append(f"{name}: nextest lists it {len(here)} times in {path}, not once")
        elif here[0]:
            problems.append(f"{name}: is #[ignore]d")


def check_pytest(collected: Path, tests: list[tuple[str, str]], problems: list[str]) -> None:
    ids = collected.read_text().splitlines()
    for name, path in tests:
        if not path.endswith(".py"):
            continue
        file = Path(path)
        if file.parts[:2] == ("compat", "tests") and file.name.startswith("test_"):
            prefix = f"{Path(*file.parts[1:])}::"
            if file.stem != name:
                prefix += name
            if not any(i.startswith(prefix) for i in ids):
                problems.append(f"{name}: pytest collects nothing for {path}")
        elif not re.search(rf"^\s*def {name}\(", (REPO / file).read_text(), re.M):
            problems.append(f"{name}: not defined in {path}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--nextest", type=Path, help="cargo nextest list JSON")
    parser.add_argument("--pytest", type=Path, help="pytest --collect-only -q output")
    args = parser.parse_args()
    problems: list[str] = []
    check_design(problems)
    tests = mapped(problems)
    if len(tests) < 25:
        problems.append(f"the map names only {len(tests)} tests; was the table reformatted?")
    if args.nextest:
        check_nextest(args.nextest, tests, problems)
    if args.pytest:
        check_pytest(args.pytest, tests, problems)
    for problem in problems:
        print(f"security-tests.md: {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
