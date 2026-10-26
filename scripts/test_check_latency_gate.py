#!/usr/bin/env python3
"""Unit tests for scripts/check-latency-gate.py (ruling M2-R17).

Run from anywhere: `python3 -I scripts/test_check_latency_gate.py`.
"""

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-latency-gate.py"
_spec = importlib.util.spec_from_file_location("check_latency_gate", SCRIPT)
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

SHAPES = ("unmerged full", "unmerged range", "merged full", "merged range")
BASE_BUILD = "b" * 40
HEAD_BUILD = "e" * 40


def record(shape: str, p50: float, p99: float, build: str = HEAD_BUILD) -> dict:
    return {
        "schema": "openxet.latency-gate.v1",
        "build": build,
        "shape": shape,
        "p50_ms": p50,
        "p99_ms": p99,
        "max_ms": p99 + 1.0,
        "runs": 200,
    }


def line(shape: str, p50: float, p99: float, build: str = HEAD_BUILD) -> str:
    return json.dumps(record(shape, p50, p99, build))


class Check(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.summary = Path(self.dir.name) / "summary.md"

    def main(self, argv: list[str], env: dict[str, str]) -> int:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return gate.main(argv, env)

    def write(self, name: str, text: str) -> Path:
        path = Path(self.dir.name) / name
        path.write_text(text)
        return path

    def run_files(self, base_text: str, head_text: str, head_build: str = HEAD_BUILD) -> tuple[int, str]:
        base_path = self.write("base.json", base_text)
        head_path = self.write("head.json", head_text)
        env = {"GITHUB_STEP_SUMMARY": str(self.summary)}
        argv = ["--base", str(base_path), "--head", str(head_path), "--head-build", head_build]
        code = self.main(argv, env)
        return code, self.summary.read_text() if self.summary.exists() else ""

    def run_gate(self, base: dict, head: dict, base_noise: str = "") -> tuple[int, str]:
        """Runs main() on {shape: (p50, p99)} maps; returns (exit, summary)."""
        return self.run_files(
            base_noise + "".join(line(s, *v, BASE_BUILD) + "\n" for s, v in base.items()),
            "".join(line(s, *v) + "\n" for s, v in head.items()),
        )

    def run_head_record(self, changes: dict) -> int:
        """Exit code when one head line differs from a valid one by `changes`."""
        rec = dict(record("merged full", 5.0, 8.0), **changes)
        base = line("merged full", 5.0, 8.0, BASE_BUILD) + "\n"
        return self.run_files(base, json.dumps(rec) + "\n")[0]

    def test_passes_when_head_matches_base(self):
        runs = {s: (20.0, 28.0) for s in SHAPES}
        code, summary = self.run_gate(runs, runs)
        self.assertEqual(code, 0)
        for shape in SHAPES:
            self.assertIn(shape, summary)
        self.assertIn("1.00", summary)

    def test_p99_regression_fails(self):
        base = {s: (20.0, 28.0) for s in SHAPES}
        head = dict(base, **{"unmerged full": (20.0, 45.0)})
        code, summary = self.run_gate(base, head)
        # 45 ms is under 50 ms, so the absolute bar alone would pass it.
        self.assertEqual(code, 0)
        head["unmerged full"] = (20.0, 51.0)
        code, summary = self.run_gate(base, head)
        self.assertEqual(code, 1)
        self.assertIn("unmerged full", summary)
        self.assertIn("p99", summary)

    def test_p50_regression_fails(self):
        base = {s: (10.0, 20.0) for s in SHAPES}
        head = dict(base, **{"merged range": (12.6, 20.0)})
        code, summary = self.run_gate(base, head)
        self.assertEqual(code, 1)
        self.assertIn("merged range", summary)
        self.assertIn("p50", summary)

    def test_p50_floor_absorbs_small_noise(self):
        # 1.25 x 4 ms leaves 1 ms; the floor allows base + 2 ms.
        base = {s: (4.0, 8.0) for s in SHAPES}
        head = dict(base, **{"merged range": (5.9, 8.0)})
        self.assertEqual(self.run_gate(base, head)[0], 0)
        head["merged range"] = (6.1, 8.0)
        code, summary = self.run_gate(base, head)
        self.assertEqual(code, 1)
        self.assertIn("p50", summary)
        # On a large p50 the ratio is the wider bound and still applies.
        base = {s: (20.0, 28.0) for s in SHAPES}
        head = dict(base, **{"unmerged full": (24.9, 28.0)})
        self.assertEqual(self.run_gate(base, head)[0], 0)
        head["unmerged full"] = (25.1, 28.0)
        self.assertEqual(self.run_gate(base, head)[0], 1)

    def test_slow_runner_passes_when_the_ratio_holds(self):
        # Issue #83: a loaded runner where nothing changed.
        base = {s: (44.0, 63.0) for s in SHAPES}
        head = {s: (46.0, 68.0) for s in SHAPES}
        code, _ = self.run_gate(base, head)
        self.assertEqual(code, 0)

    def test_fast_runner_keeps_the_absolute_bar(self):
        # 1.25 x 30 ms is 37.5 ms, but 50 ms is the bar while the runner
        # can meet it: a fast runner passes up to 50 ms...
        base = {s: (10.0, 30.0) for s in SHAPES}
        head = dict(base, **{"unmerged full": (10.0, 49.0)})
        self.assertEqual(self.run_gate(base, head)[0], 0)
        # ...and fails past it.
        head["unmerged full"] = (10.0, 50.5)
        self.assertEqual(self.run_gate(base, head)[0], 1)

    def test_base_without_measurements_holds_head_to_the_absolute_bar(self):
        # A base that can't build HEAD's gate test prints no JSON lines.
        head = {s: (20.0, 28.0) for s in SHAPES}
        code, summary = self.run_gate({}, head, base_noise="test output\n")
        self.assertEqual(code, 0)
        self.assertIn("absolute", summary)
        head["unmerged full"] = (20.0, 55.0)
        self.assertEqual(self.run_gate({}, head)[0], 1)

    def test_shape_missing_from_head_fails(self):
        base = {s: (20.0, 28.0) for s in SHAPES}
        head = {s: v for s, v in base.items() if s != "merged full"}
        code, summary = self.run_gate(base, head)
        self.assertEqual(code, 1)
        self.assertIn("merged full", summary)

    def test_empty_head_is_an_input_error(self):
        base = {s: (20.0, 28.0) for s in SHAPES}
        self.assertEqual(self.run_gate(base, {})[0], 2)

    def test_wrong_schema_is_an_input_error(self):
        self.assertEqual(self.run_head_record({"schema": "openxet.latency-gate.v0"}), 2)

    def test_non_numeric_negative_or_non_finite_values_are_input_errors(self):
        for bad in ("20", True, None, [1.0], -1.0):
            with self.subTest(value=bad):
                self.assertEqual(self.run_head_record({"p50_ms": bad}), 2)
        self.assertEqual(self.run_head_record({"runs": 1.5}), 2)
        self.assertEqual(self.run_head_record({"shape": 7}), 2)
        base = line("merged full", 5.0, 8.0, BASE_BUILD) + "\n"
        for literal in ("NaN", "Infinity", "-Infinity"):
            with self.subTest(value=literal):
                head = line("merged full", 5.0, 8.0).replace('"p99_ms": 8.0', f'"p99_ms": {literal}')
                self.assertIn(literal, head)
                self.assertEqual(self.run_files(base, head + "\n")[0], 2)

    def test_duplicate_shape_is_an_input_error(self):
        base = line("merged full", 5.0, 8.0, BASE_BUILD) + "\n"
        head = (line("merged full", 5.0, 8.0) + "\n") * 2
        self.assertEqual(self.run_files(base, head)[0], 2)

    def test_p99_at_50_ms_fails_as_assert_mode_does(self):
        head = {s: (10.0, 28.0) for s in SHAPES}
        head["unmerged full"] = (10.0, 50.0)
        self.assertEqual(self.run_gate({}, head)[0], 1)
        self.assertEqual(self.run_gate({s: (10.0, 30.0) for s in SHAPES}, head)[0], 1)
        # A base too slow for 50 ms holds the PR to 1.25 x its p99.
        self.assertEqual(self.run_gate({s: (10.0, 60.0) for s in SHAPES}, head)[0], 0)

    def test_zero_base_p50_uses_the_floor(self):
        base = {s: (0.0, 8.0) for s in SHAPES}
        head = dict(base, **{"merged range": (1.5, 8.0)})
        code, summary = self.run_gate(base, head)
        self.assertEqual(code, 0)
        self.assertIn("n/a", summary)
        head["merged range"] = (2.5, 8.0)
        self.assertEqual(self.run_gate(base, head)[0], 1)

    def test_non_json_lines_are_skipped(self):
        noise = "    Compiling openxet-cas\nunmerged full: p50 20.2 ms\n"
        base = noise + "".join(line(s, 20.0, 28.0, BASE_BUILD) + "\n" for s in SHAPES)
        head = noise + "".join(line(s, 20.0, 28.0) + "\n" for s in SHAPES)
        self.assertEqual(self.run_files(base, head)[0], 0)

    def test_base_and_head_from_the_same_build_are_an_input_error(self):
        # Both runs measuring one tree would pass any regression.
        same = "".join(line(s, 20.0, 28.0) + "\n" for s in SHAPES)
        code, _ = self.run_files(same, same)
        self.assertEqual(code, 2)

    def test_head_from_another_build_is_an_input_error(self):
        head = "".join(line(s, 20.0, 28.0) + "\n" for s in SHAPES)
        base = "".join(line(s, 20.0, 28.0, BASE_BUILD) + "\n" for s in SHAPES)
        self.assertEqual(self.run_files(base, head, head_build="f" * 40)[0], 2)

    def test_missing_or_mixed_builds_are_input_errors(self):
        self.assertEqual(self.run_head_record({"build": None}), 2)
        base = "".join(line(s, 20.0, 28.0, BASE_BUILD) + "\n" for s in SHAPES)
        mixed = line("merged full", 5.0, 8.0) + "\n" + line("merged range", 5.0, 8.0, "c" * 40) + "\n"
        self.assertEqual(self.run_files(base, mixed)[0], 2)

    def test_p99_limit_sets_the_absolute_bar(self):
        # The Hub metadata gate holds token, resolve and tree to 10 ms.
        hub = ("token", "resolve head xet", "resolve head regular", "tree")
        base_path = self.write("base.json", "".join(line(s, 3.0, 6.0, BASE_BUILD) + "\n" for s in hub))
        head = {s: (3.0, 6.0) for s in hub}

        def run(head: dict) -> tuple[int, str]:
            head_path = self.write("head.json", "".join(line(s, *v) + "\n" for s, v in head.items()))
            argv = [
                "--base", str(base_path), "--head", str(head_path), "--head-build", HEAD_BUILD,
                "--p99-limit-ms", "10", "--title", "Hub metadata latency gate",
            ]
            code = self.main(argv, {"GITHUB_STEP_SUMMARY": str(self.summary)})
            return code, self.summary.read_text()

        self.assertEqual(run(dict(head, tree=(3.0, 9.9)))[0], 0)
        code, summary = run(dict(head, tree=(3.0, 10.0)))
        self.assertEqual(code, 1)
        self.assertIn("## Hub metadata latency gate", summary)
        self.assertIn("p99 >= 10 ms", summary)

    def test_p99_limit_must_be_positive(self):
        base = "".join(line(s, 20.0, 28.0, BASE_BUILD) + "\n" for s in SHAPES)
        head = "".join(line(s, 20.0, 28.0) + "\n" for s in SHAPES)
        argv = [
            "--base", str(self.write("b.json", base)), "--head", str(self.write("h.json", head)),
            "--head-build", HEAD_BUILD, "--p99-limit-ms", "0",
        ]
        with self.assertRaises(SystemExit) as raised:
            self.main(argv, {})
        self.assertEqual(raised.exception.code, 2)

    def test_no_summary_file_without_github(self):
        base_path = self.write("base.json", "".join(line(s, 20.0, 28.0, BASE_BUILD) + "\n" for s in SHAPES))
        head_path = self.write("head.json", "".join(line(s, 20.0, 28.0) + "\n" for s in SHAPES))
        argv = ["--base", str(base_path), "--head", str(head_path), "--head-build", HEAD_BUILD]
        self.assertEqual(self.main(argv, {}), 0)
        self.assertFalse(self.summary.exists())


if __name__ == "__main__":
    unittest.main()
