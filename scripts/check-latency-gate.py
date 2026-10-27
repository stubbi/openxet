#!/usr/bin/env python3
"""Compares a pull request's latency gate run with its base's, measured in
the same CI job on the same runner (ruling M2-R17, issue #83). The base is
the base branch's tip recorded in the `pull_request` event.

Each input holds the JSON lines a latency gate prints, one per shape, schema
`openxet.latency-gate.v1`: the reconstruction gate
(`crates/cas/tests/latency_gate.rs`, design §5.8's 50 ms, the default bar)
or the Hub metadata gate (`crates/hub/tests/hub_latency_gate.rs`, 10 ms,
`--p99-limit-ms 10`). Lines that don't start with `{` are skipped. Both runs use HEAD's measurement code: CI copies HEAD's gate
test and its fixture into the base checkout, so only the measured library
differs. Each line's `build` is the commit its test binary was compiled from
(`OPENXET_LATENCY_GATE_BUILD` at build time); the head's must equal
`--head-build` and the base's must differ, so two runs of one build can't
pass as a comparison. A shape fails when

    head.p99 >= bar  and  head.p99 > 1.25 x base.p99,  or
    head.p50 > max(1.25 x base.p50, base.p50 + 2 ms)

so a runner slow enough to miss design §5.8's bar on the base doesn't fail
an unchanged PR, while a runner that can meet the bar still holds the PR to
it, failing at the bar as the test's assert mode does. The 2 ms p50 floor
keeps a shape whose p50 is a few milliseconds from failing on scheduler
noise between the two runs. A shape the base didn't measure (a base that
can't build HEAD's test, or a new shape) is held to the absolute bar alone;
a shape the base measured and the head didn't fails.

Prints a table of both runs and the ratios, and appends it to
`$GITHUB_STEP_SUMMARY` when that is set. Exits 0 on a pass, 1 on a
regression, 2 when an input can't be read or the builds don't check out.
"""

import argparse
import json
import math
import os
import sys
from pathlib import Path

SCHEMA = "openxet.latency-gate.v1"
#: Design §5.8: reconstruction's bar, the default.
P99_LIMIT_MS = 50.0
#: Ruling M2-R17.
MAX_RATIO = 1.25
#: Ruling M2-R17: the least p50 growth that fails.
P50_FLOOR_MS = 2.0


class InputError(Exception):
    pass


def _reject_constant(name: str):
    raise ValueError(f"{name} is not a finite number")


def _number(record: dict, key: str, kind: tuple) -> float:
    value = record.get(key)
    # bool is an int in Python, never a measurement.
    if isinstance(value, bool) or not isinstance(value, kind):
        raise ValueError(f"field {key!r} must be a number, not {value!r}")
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"field {key!r} must be finite and non-negative, not {value!r}")
    return value


def load(path: Path) -> tuple[dict[str, dict], str | None]:
    """({shape: line}, build) from one run's output."""
    try:
        text = path.read_text()
    except OSError as e:
        raise InputError(f"{path}: {e.strerror}") from e
    runs, builds = {}, set()
    for number, raw in enumerate(text.splitlines(), 1):
        if not raw.startswith("{"):
            continue
        where = f"{path}:{number}"
        try:
            record = json.loads(raw, parse_constant=_reject_constant)
        except ValueError as e:
            raise InputError(f"{where}: not valid JSON ({e})") from e
        if not isinstance(record, dict) or record.get("schema") != SCHEMA:
            raise InputError(
                f"{where}: expected schema {SCHEMA}; rebuild both runs "
                "from a tree whose latency_gate.rs prints it"
            )
        try:
            shape = record.get("shape")
            if not isinstance(shape, str) or not shape:
                raise ValueError(f"field 'shape' must be a non-empty string, not {shape!r}")
            build = record.get("build")
            if not isinstance(build, str) or not build:
                raise ValueError(
                    "field 'build' is missing; build the gate with "
                    "OPENXET_LATENCY_GATE_BUILD set to the tree's commit"
                )
            for key in ("p50_ms", "p99_ms", "max_ms"):
                _number(record, key, (int, float))
            _number(record, "runs", (int,))
        except ValueError as e:
            raise InputError(f"{where}: {e}") from e
        if shape in runs:
            raise InputError(f"{where}: shape {shape!r} appears twice")
        runs[shape] = record
        builds.add(build)
    if len(builds) > 1:
        raise InputError(f"{path}: lines from several builds ({', '.join(sorted(builds))})")
    return runs, next(iter(builds), None)


def positive_ms(text: str) -> float:
    value = float(text)
    if not math.isfinite(value) or value <= 0:
        raise argparse.ArgumentTypeError(f"{text!r} is not a positive number of milliseconds")
    return value


def ratio(head: float, base: float) -> str:
    return f"{head / base:.2f}" if base > 0 else "n/a"


def compare(
    base: dict[str, dict], head: dict[str, dict], p99_limit: float = P99_LIMIT_MS
) -> tuple[list[str], list[str]]:
    """(table lines, failures)."""
    table = [
        "| Shape | Base p50 / p99 (ms) | Head p50 / p99 (ms) | p50 ratio | p99 ratio | p99 bar (ms) | Result |",
        "|---|---|---|---|---|---|---|",
    ]
    failures = []
    for shape, h in head.items():
        h50, h99 = float(h["p50_ms"]), float(h["p99_ms"])
        b = base.get(shape)
        problems = []
        if b is None:
            p99_bar = p99_limit
            p99_over = h99 >= p99_limit
            base_cell, r50, r99 = "not measured (absolute bar only)", "n/a", "n/a"
        else:
            b50, b99 = float(b["p50_ms"]), float(b["p99_ms"])
            p99_bar = max(p99_limit, MAX_RATIO * b99)
            p99_over = h99 >= p99_limit and h99 > MAX_RATIO * b99
            base_cell = f"{b50:.1f} / {b99:.1f}"
            r50, r99 = ratio(h50, b50), ratio(h99, b99)
            p50_bar = max(MAX_RATIO * b50, b50 + P50_FLOOR_MS)
            if h50 > p50_bar:
                problems.append(
                    f"p50 {h50:.1f} ms > bar {p50_bar:.1f} ms (base {b50:.1f} ms)"
                )
        if p99_over:
            problems.append(f"p99 {h99:.1f} ms past bar {p99_bar:.1f} ms")
        failures += [f"{shape}: {p}" for p in problems]
        table.append(
            f"| {shape} | {base_cell} | {h50:.1f} / {h99:.1f} | {r50} | {r99} "
            f"| {p99_bar:.1f} | {'fail' if problems else 'pass'} |"
        )
    for shape in base.keys() - head.keys():
        failures.append(f"{shape}: measured on the base but not on the head")
        table.append(f"| {shape} | measured | missing | | | | fail |")
    return table, failures


def main(argv: list[str], env: dict[str, str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--base", type=Path, required=True, help="the base run's JSON lines")
    parser.add_argument("--head", type=Path, required=True, help="the pull request run's JSON lines")
    parser.add_argument(
        "--head-build",
        required=True,
        help="the commit the head run must come from (in CI, the checked-out merge commit)",
    )
    parser.add_argument(
        "--p99-limit-ms",
        type=positive_ms,
        default=P99_LIMIT_MS,
        help=f"the absolute p99 bar in ms (default {P99_LIMIT_MS:g}, design §5.8's reconstruction bar)",
    )
    parser.add_argument(
        "--title",
        default="Reconstruction latency gate",
        help="the report's heading",
    )
    args = parser.parse_args(argv)
    limit = args.p99_limit_ms
    try:
        (base, base_build), (head, head_build) = load(args.base), load(args.head)
        if not head:
            raise InputError(
                f"{args.head}: no {SCHEMA} lines; run the gate with "
                "OPENXET_LATENCY_GATE=measure and --no-capture"
            )
        if head_build != args.head_build:
            raise InputError(
                f"{args.head}: built from {head_build}, not {args.head_build}; the head "
                "run measured another tree (a stale build in a shared target directory?)"
            )
        if base_build == head_build:
            raise InputError(
                f"{args.base} and {args.head} are both from {head_build}: the comparison "
                "would measure one tree twice; give each tree its own target directory"
            )
    except InputError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    table, failures = compare(base, head, limit)
    rule = (
        f"A shape fails when head p99 >= {limit:g} ms and > {MAX_RATIO} x base p99, "
        f"or head p50 > max({MAX_RATIO} x base p50, base p50 + {P50_FLOOR_MS:g} ms) "
        "(ruling M2-R17)."
    )
    if failures:
        verdict = "**Latency gate failed:**\n\n" + "".join(f"- {f}\n" for f in failures)
    else:
        verdict = "**Latency gate passed.**\n"
    if not base:
        verdict += (
            "\nThe base printed no measurements, so every shape is held "
            f"to the absolute {limit:g} ms bar.\n"
        )
    builds = f"Base build `{base_build or 'none'}`, head build `{head_build}`.\n\n"
    report = f"## {args.title}\n\n" + builds + "\n".join(table) + f"\n\n{rule}\n\n{verdict}"
    print(report)
    summary = env.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(report + "\n")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], dict(os.environ)))
