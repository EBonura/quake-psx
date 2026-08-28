#!/usr/bin/env python3
"""Aggregate PSoXide PC samples using a rust-lld guest link map.

The PS-X EXE is a flat binary, but rust-lld's ``-Map`` output retains exact
function ranges without changing one byte of the image. This accepts either
the aggregate ``--pc-sample-log`` CSV or the windowed form and attributes each
sample to the smallest enclosing named map range.
"""

from __future__ import annotations

import argparse
import bisect
import csv
import re
from dataclasses import dataclass
from pathlib import Path


MAP_ROW = re.compile(
    r"^\s*([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+\d+\s+(.+?)\s*$"
)


@dataclass(frozen=True)
class Symbol:
    start: int
    end: int
    name: str


def load_symbols(path: Path) -> list[Symbol]:
    """Read exact named function ranges from a rust-lld map."""

    by_range: dict[tuple[int, int], str] = {}
    for line in path.read_text().splitlines():
        match = MAP_ROW.match(line)
        if not match:
            continue
        start = int(match.group(1), 16)
        size = int(match.group(3), 16)
        name = match.group(4).strip()
        if size == 0 or start < 0x80010000:
            continue
        # Input-section rows duplicate the following readable symbol row.
        if ":(" in name or name.startswith(".") or name.endswith(" = ."):
            continue
        by_range[(start, start + size)] = name
    symbols = [Symbol(start, end, name) for (start, end), name in by_range.items()]
    symbols.sort(key=lambda symbol: (symbol.start, symbol.end - symbol.start))
    return symbols


def enclosing_symbol(symbols: list[Symbol], starts: list[int], pc: int) -> Symbol | None:
    """Return the narrowest named range containing ``pc``."""

    cursor = bisect.bisect_right(starts, pc)
    best = None
    for index in range(cursor - 1, max(-1, cursor - 16), -1):
        candidate = symbols[index]
        if candidate.start <= pc < candidate.end:
            if best is None or candidate.end - candidate.start < best.end - best.start:
                best = candidate
    return best


def aggregate(
    symbols: list[Symbol],
    samples_path: Path,
    minimum_window_start: int | None,
    maximum_window_start: int | None = None,
) -> tuple[int, dict[str, int], dict[str, tuple[int, int]]]:
    starts = [symbol.start for symbol in symbols]
    totals: dict[str, int] = {}
    hottest: dict[str, tuple[int, int]] = {}
    grand = 0
    with samples_path.open(newline="") as source:
        for row in csv.DictReader(source):
            if minimum_window_start is not None:
                if "window_start_tick" not in row:
                    raise ValueError("--min-window-start requires a windowed sample CSV")
                if int(row["window_start_tick"]) < minimum_window_start:
                    continue
            if maximum_window_start is not None:
                if "window_start_tick" not in row:
                    raise ValueError("--max-window-start requires a windowed sample CSV")
                if int(row["window_start_tick"]) > maximum_window_start:
                    continue
            pc = int(row["pc"], 16)
            count = int(row["samples"])
            symbol = enclosing_symbol(symbols, starts, pc)
            name = symbol.name if symbol is not None else "<unmapped>"
            totals[name] = totals.get(name, 0) + count
            if count > hottest.get(name, (0, 0))[0]:
                hottest[name] = (count, pc)
            grand += count
    return grand, totals, hottest


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--map", required=True, type=Path)
    parser.add_argument("--samples", required=True, type=Path)
    parser.add_argument("--top", type=int, default=40)
    parser.add_argument("--min-window-start", type=int)
    parser.add_argument("--max-window-start", type=int)
    args = parser.parse_args()

    symbols = load_symbols(args.map)
    if not symbols:
        parser.error("the link map contains no named function ranges")
    try:
        grand, totals, hottest = aggregate(
            symbols,
            args.samples,
            args.min_window_start,
            args.max_window_start,
        )
    except ValueError as error:
        parser.error(str(error))
    ranked = sorted(totals.items(), key=lambda item: (-item[1], item[0]))[: args.top]

    print(f"{grand} samples over {len(totals)} symbols")
    print(f"{'samples':>9} {'pct':>7}  {'hot pc':>10}  symbol")
    for name, count in ranked:
        _, pc = hottest[name]
        print(f"{count:>9} {100 * count / grand:>6.2f}%  0x{pc:08x}  {name}")


if __name__ == "__main__":
    main()
