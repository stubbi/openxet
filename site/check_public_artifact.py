#!/usr/bin/env python3
"""Check the final Pages tree before publishing with hidden files included."""

import os
import re
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

HIDDEN_FILES = {".well-known/security.txt", "docs/.nojekyll"}
HIDDEN_DIRECTORIES = {".well-known"}
TIMESTAMP = re.compile(r"\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-](?:[01]\d|2[0-3]):[0-5]\d)")


def check(public: Path) -> list[str]:
    if public.is_symlink() or not public.is_dir():
        return ["public artifact must be a real directory; rebuild the site"]

    problems = []
    for directory, dirs, files in os.walk(public, followlinks=False, onerror=lambda error: problems.append(
        f"cannot inspect artifact directory {error.filename!r}; check its permissions and rebuild"
    )):
        for name in sorted(dirs + files):
            path = Path(directory) / name
            relative = path.relative_to(public)
            # Pages' archive action dereferences links, including visible ones.
            if path.is_symlink():
                problems.append(f"artifact contains symlink {relative.as_posix()!r}; publish a regular file instead")
            elif any(part.startswith(".") for part in relative.parts):
                allowed = (relative.as_posix() in HIDDEN_FILES and path.is_file()) or (
                    relative.as_posix() in HIDDEN_DIRECTORIES and path.is_dir()
                )
                if not allowed:
                    problems.append(f"unexpected hidden artifact path {relative.as_posix()!r}; remove it before publishing")
            elif not (path.is_file() or path.is_dir()):
                problems.append(f"artifact path {relative.as_posix()!r} is not a regular file or directory; remove it")
    if problems:
        return problems

    security = public / ".well-known/security.txt"
    if not security.is_file():
        return ["artifact is missing .well-known/security.txt; restore the published security contact"]
    try:
        text = security.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return ["cannot read security.txt as UTF-8; restore the published security contact"]
    values = [line.partition(":")[2].strip() for line in text.splitlines() if line.partition(":")[0].lower() == "expires"]
    # RFC 9116 §2.5.5 requires one RFC 3339 expiry and recommends less than a year.
    if len(values) != 1 or not TIMESTAMP.fullmatch(values[0]):
        return ["security.txt needs exactly one Expires timestamp with a timezone; review the contact and renew it"]
    try:
        expiry = datetime.fromisoformat(values[0].upper().replace("Z", "+00:00"))
    except ValueError:
        return ["security.txt Expires is invalid; review the contact and renew it"]
    now = datetime.now(timezone.utc)
    if not now < expiry < now + timedelta(days=365):
        return ["security.txt Expires must be in the future and less than a year away; review the contact and renew it"]
    return []


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: check_public_artifact.py PUBLIC_DIRECTORY", file=sys.stderr)
        return 1
    problems = check(Path(sys.argv[1]))
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
