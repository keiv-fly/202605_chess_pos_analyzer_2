#!/usr/bin/env python3
"""Extract the first 100 Lichess puzzles to a small CSV file."""

from __future__ import annotations

import argparse
import csv
import io
import sys
import tempfile
import zipfile
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator, TextIO


DEFAULT_ARCHIVE = "lichess_db_puzzle.csv.zst"
DEFAULT_OUTPUT = "lichess_db_puzzle_top_100.csv"
DEFAULT_COUNT = 100


@contextmanager
def open_puzzle_csv(path: Path) -> Iterator[TextIO]:
    suffixes = "".join(path.suffixes).lower()

    if suffixes.endswith(".csv.zst"):
        try:
            import zstandard
        except ImportError as exc:
            raise RuntimeError(
                "Reading .zst files requires the Python package 'zstandard'. "
                "Install it with: python -m pip install zstandard"
            ) from exc

        with path.open("rb") as compressed:
            reader = zstandard.ZstdDecompressor().stream_reader(compressed)
            text = io.TextIOWrapper(reader, encoding="utf-8", newline="")
            try:
                yield text
            finally:
                text.detach()
                reader.close()
        return

    if suffixes.endswith(".zip"):
        with zipfile.ZipFile(path) as archive:
            csv_members = [
                name
                for name in archive.namelist()
                if name.lower().endswith(".csv") and not name.endswith("/")
            ]
            if not csv_members:
                raise RuntimeError(f"No CSV file found inside {path}")

            with archive.open(csv_members[0], "r") as compressed:
                text = io.TextIOWrapper(compressed, encoding="utf-8", newline="")
                yield text
        return

    if suffixes.endswith(".csv"):
        with path.open("r", encoding="utf-8", newline="") as text:
            yield text
        return

    raise RuntimeError(f"Unsupported puzzle file type: {path}")


def extract_rows(source: Path, destination: Path, count: int) -> int:
    if count < 1:
        raise ValueError("--count must be at least 1")
    if not source.exists():
        raise FileNotFoundError(source)

    destination.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.NamedTemporaryFile(
        delete=False,
        dir=destination.parent,
        prefix=f"{destination.name}.",
        suffix=".part",
        mode="w",
        encoding="utf-8",
        newline="",
    ) as temp_file:
        temp_path = Path(temp_file.name)

    written = 0

    try:
        with open_puzzle_csv(source) as input_file, temp_path.open(
            "w", encoding="utf-8", newline=""
        ) as output_file:
            reader = csv.reader(input_file)
            writer = csv.writer(output_file)

            header = next(reader, None)
            if header is None:
                raise RuntimeError(f"No rows found in {source}")

            writer.writerow(header)

            for row in reader:
                if written >= count:
                    break
                writer.writerow(row)
                written += 1

        temp_path.replace(destination)
    except Exception:
        temp_path.unlink(missing_ok=True)
        raise

    return written


def parse_args() -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[1]
    puzzles_dir = repo_root / "puzzles"

    parser = argparse.ArgumentParser(
        description="Extract the first puzzles from the compressed Lichess puzzle database."
    )
    parser.add_argument(
        "--source",
        type=Path,
        default=puzzles_dir / DEFAULT_ARCHIVE,
        help=f"Compressed puzzle database path. Default: {puzzles_dir / DEFAULT_ARCHIVE}",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=puzzles_dir / DEFAULT_OUTPUT,
        help=f"Output CSV path. Default: {puzzles_dir / DEFAULT_OUTPUT}",
    )
    parser.add_argument(
        "--count",
        type=int,
        default=DEFAULT_COUNT,
        help=f"Number of puzzle rows to extract. Default: {DEFAULT_COUNT}",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    try:
        written = extract_rows(args.source, args.output, args.count)
    except Exception as exc:
        print(f"Extraction failed: {exc}", file=sys.stderr)
        return 1

    print(f"Wrote {written} puzzles to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
