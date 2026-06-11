#!/usr/bin/env python3
"""Download the Lichess puzzle database into the local puzzles directory."""

from __future__ import annotations

import argparse
import sys
import tempfile
import urllib.request
from pathlib import Path


PUZZLE_URL = "https://database.lichess.org/lichess_db_puzzle.csv.zst"
DEFAULT_FILENAME = "lichess_db_puzzle.csv.zst"
TEST_BYTES = 1024 * 1024


def format_size(num_bytes: int) -> str:
    units = ("B", "KiB", "MiB", "GiB")
    size = float(num_bytes)
    for unit in units:
        if size < 1024 or unit == units[-1]:
            return f"{size:.1f} {unit}" if unit != "B" else f"{num_bytes} {unit}"
        size /= 1024
    return f"{num_bytes} B"


def download(url: str, destination: Path, max_bytes: int | None = None) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.NamedTemporaryFile(
        delete=False,
        dir=destination.parent,
        prefix=f"{destination.name}.",
        suffix=".part",
    ) as temp_file:
        temp_path = Path(temp_file.name)

    try:
        print(f"Downloading {url}")
        print(f"Destination: {destination}")
        if max_bytes is not None:
            print(f"Test mode: downloading up to {format_size(max_bytes)}")

        with urllib.request.urlopen(url) as response, temp_path.open("wb") as output:
            content_length = response.headers.get("Content-Length")
            total = int(content_length) if content_length else None
            downloaded = 0

            while True:
                read_size = 1024 * 1024
                if max_bytes is not None:
                    remaining = max_bytes - downloaded
                    if remaining <= 0:
                        break
                    read_size = min(read_size, remaining)

                chunk = response.read(read_size)
                if not chunk:
                    break
                output.write(chunk)
                downloaded += len(chunk)

                progress_total = min(total, max_bytes) if total and max_bytes else total

                if progress_total:
                    percent = downloaded / progress_total * 100
                    print(
                        f"\r{format_size(downloaded)} / {format_size(progress_total)} "
                        f"({percent:.1f}%)",
                        end="",
                        flush=True,
                    )
                else:
                    print(f"\r{format_size(downloaded)}", end="", flush=True)

        print()
        temp_path.replace(destination)
        print(f"Downloaded: {destination}")
    except Exception:
        temp_path.unlink(missing_ok=True)
        raise


def parse_args() -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[1]
    default_output_dir = repo_root / "puzzles"

    parser = argparse.ArgumentParser(
        description="Download the Lichess puzzle database archive."
    )
    parser.add_argument(
        "--url",
        default=PUZZLE_URL,
        help=f"Puzzle database URL. Default: {PUZZLE_URL}",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=default_output_dir,
        help=f"Directory to store the puzzle archive. Default: {default_output_dir}",
    )
    parser.add_argument(
        "--filename",
        default=DEFAULT_FILENAME,
        help=f"Downloaded archive filename. Default: {DEFAULT_FILENAME}",
    )
    parser.add_argument(
        "--test",
        action="store_true",
        help=(
            "Download a small sample to a .test file instead of the full archive."
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    destination = args.output_dir / args.filename
    max_bytes = None
    if args.test:
        destination = destination.with_name(f"{destination.name}.test")
        max_bytes = TEST_BYTES

    try:
        download(args.url, destination, max_bytes)
    except KeyboardInterrupt:
        print("\nDownload cancelled.", file=sys.stderr)
        return 130
    except Exception as exc:
        print(f"Download failed: {exc}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
