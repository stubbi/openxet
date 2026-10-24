"""`docs/reference/security-tests.md` stays true to the code and the design.

Every test the page maps to a file is defined there, and nowhere else under
the same name, so a rename or a move fails here instead of leaving the map
pointing at nothing. Every test name in design §5.9 is on the page.
"""

import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MAP = REPO / "docs/reference/security-tests.md"
DESIGN = REPO / "docs/superpowers/specs/2026-10-23-openxet-design.md"

_NAME = re.compile(r"^`([a-z0-9_]+)`$")
_PATH = re.compile(r"^`([\w./-]+\.(?:rs|py))`$")


def _rows(text: str) -> list[list[str]]:
    return [
        [cell.strip() for cell in line.strip().strip("|").split("|")]
        for line in text.splitlines()
        if line.startswith("| ") and not line.startswith("| ---")
    ]


def _mapped() -> list[tuple[str, str]]:
    """Each (test, file) the page maps, for tests that are written."""
    mapped = []
    for row in _rows(MAP.read_text()):
        name, path = _NAME.match(row[1]), _PATH.match(row[2])
        if name and path:
            mapped.append((name.group(1), path.group(1)))
    return mapped


def _definitions(name: str, text: str, suffix: str) -> int:
    keyword = "fn" if suffix == ".rs" else "def"
    return len(re.findall(rf"\b{keyword} {name}\s*\(", text))


def test_the_map_names_tests_and_files():
    # Guards the parsing below: a reformatted table must not pass by
    # matching nothing.
    assert len(_mapped()) >= 25


def test_every_mapped_test_is_defined_once_in_its_file():
    for name, path in _mapped():
        file = REPO / path
        assert file.is_file(), f"{name}: {path} does not exist"
        if file.stem == name:
            # A Python test module named after the requirement.
            continue
        count = _definitions(name, file.read_text(), file.suffix)
        assert count == 1, f"{name} is defined {count} times in {path}"


def test_every_mapped_rust_test_name_is_unique_in_the_workspace():
    sources = [p for p in (REPO / "crates").rglob("*.rs") if "target" not in p.parts]
    texts = {p: p.read_text() for p in sources}
    for name, path in _mapped():
        if not path.endswith(".rs"):
            continue
        holders = [
            str(p.relative_to(REPO))
            for p, text in texts.items()
            if _definitions(name, text, ".rs")
        ]
        assert holders == [path], f"{name} is defined in {holders}"


def test_every_test_named_in_design_5_9_is_mapped():
    design = DESIGN.read_text()
    section = design.split("### 5.9 Security requirements", 1)[1].split("\n## ", 1)[0]
    named = set()
    for row in _rows(section):
        if row[0].isdigit():
            named.update(re.findall(r"`([a-z0-9_]+)`", row[-1]))
    assert named, "no test names found in design §5.9"
    on_page = {
        m.group(1)
        for row in _rows(MAP.read_text())
        if (m := re.match(r"^`([a-z0-9_]+)`", row[1]))
    }
    missing = sorted(named - on_page)
    assert not missing, f"design §5.9 names tests the map lacks: {missing}"
    assert "security-tests.md" in section, "design §5.9 does not link the map"
