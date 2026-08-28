#!/usr/bin/env python3
"""Summarize PSoXide GP0 counters over complete gameplay presentations."""

from __future__ import annotations

import argparse
import csv
import json
import math
from pathlib import Path
from typing import Iterable


GPU_FIELDS = (
    "frame_draw_words",
    "commands",
    "draws",
    "fills",
    "textured_tris",
    "textured_quads",
    "textured_rects",
    "texture_windows",
    "texture_window_changes",
    "texture_window_redundant",
    "gpu_cycles",
    "fill_cycles",
    "textured_tri_cycles",
    "textured_quad_cycles",
    "textured_rect_cycles",
    "other_cycles",
)


def integer(value: str | None) -> int:
    if not value:
        return 0
    return int(value, 0)


def read_rows(path: Path) -> dict[int, dict[str, int]]:
    with path.open(newline="") as source:
        rows = {}
        for raw in csv.DictReader(source):
            tick = integer(raw.get("route_tick"))
            rows[tick] = {
                key: integer(value)
                for key, value in raw.items()
                if key != "route_tick" and value is not None
            }
        return rows


def readn_cycles(path: Path) -> list[int]:
    with path.open(newline="") as source:
        rows = csv.reader(source)
        next(rows, None)
        return [int(row[0]) for row in rows if len(row) > 1 and row[1] == "0x06"]


def gameplay_bounds(route: dict[int, dict[str, int]], cd_path: Path) -> tuple[int, int]:
    reads = readn_cycles(cd_path)
    if len(reads) < 2:
        raise ValueError("CD log has fewer than two ReadN sessions")
    initial_last, transition = max(zip(reads, reads[1:]), key=lambda pair: pair[1] - pair[0])
    presents = [
        tick
        for tick, row in sorted(route.items())
        if row.get("display_start_changed", 0)
        and initial_last < row.get("bus_cycles", 0) < transition
    ]
    if len(presents) < 2:
        raise ValueError("gameplay window has fewer than two presentations")
    return presents[0], presents[-1]


def nearest_rank(values: list[int], fraction: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def distribution(values: Iterable[int]) -> dict[str, float | int]:
    data = list(values)
    if not data:
        return {"mean": 0.0, "p50": 0, "p95": 0, "max": 0}
    return {
        "mean": sum(data) / len(data),
        "p50": nearest_rank(data, 0.50),
        "p95": nearest_rank(data, 0.95),
        "max": max(data),
    }


def summarize(
    gpu: dict[int, dict[str, int]],
    route: dict[int, dict[str, int]],
    first_present: int,
    last_present: int,
) -> dict[str, object]:
    presents = [
        tick
        for tick, row in sorted(route.items())
        if row.get("display_start_changed", 0) and first_present <= tick <= last_present
    ]
    frames = list(zip(presents, presents[1:]))
    if not frames:
        raise ValueError("selected window has no complete presentation intervals")

    def values(field: str) -> list[int]:
        return [
            sum(gpu.get(tick, {}).get(field, 0) for tick in range(previous + 1, current + 1))
            for previous, current in frames
        ]

    per_present = {field: distribution(values(field)) for field in GPU_FIELDS}
    totals = {field: sum(values(field)) for field in GPU_FIELDS}
    gpu_cycles = totals["gpu_cycles"]
    cycle_share = {
        field: 100.0 * totals[field] / gpu_cycles if gpu_cycles else 0.0
        for field in (
            "fill_cycles",
            "textured_tri_cycles",
            "textured_quad_cycles",
            "textured_rect_cycles",
            "other_cycles",
        )
    }
    return {
        "first_present_tick": first_present,
        "last_present_tick": last_present,
        "complete_present_intervals": len(frames),
        "per_present": per_present,
        "totals": totals,
        "gpu_cycle_share_percent": cycle_share,
        "notes": [
            "GP0 counts are direct PSoXide observations.",
            "GPU cycle fields are emulator estimates, not original-silicon timing.",
        ],
    }


def print_text(summary: dict[str, object]) -> None:
    print(
        "PSoXide GP0 gameplay census: "
        f"ticks={summary['first_present_tick']}..{summary['last_present_tick']} "
        f"presents={summary['complete_present_intervals']}"
    )
    per_present = summary["per_present"]
    assert isinstance(per_present, dict)
    for field in GPU_FIELDS:
        row = per_present[field]
        print(
            f"{field}: mean={row['mean']:.2f} p50={row['p50']} "
            f"p95={row['p95']} max={row['max']}"
        )
    shares = summary["gpu_cycle_share_percent"]
    assert isinstance(shares, dict)
    print(
        "gpu cycle share: "
        + " ".join(f"{field}={value:.2f}%" for field, value in shares.items())
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("gpu_csv", type=Path)
    parser.add_argument("route_csv", type=Path)
    parser.add_argument("cd_csv", type=Path)
    parser.add_argument("--first-present", type=int)
    parser.add_argument("--last-present", type=int)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    gpu = read_rows(args.gpu_csv)
    route = read_rows(args.route_csv)
    automatic_first, automatic_last = gameplay_bounds(route, args.cd_csv)
    summary = summarize(
        gpu,
        route,
        args.first_present if args.first_present is not None else automatic_first,
        args.last_present if args.last_present is not None else automatic_last,
    )
    if args.json:
        print(json.dumps(summary, indent=2, sort_keys=True))
    else:
        print_text(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
