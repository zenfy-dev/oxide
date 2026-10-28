#!/usr/bin/env python3
"""Compare release binaries on ZIP listing and large extraction selections.

Uses only the Python standard library. Fixtures and outputs are temporary.
Filtering selects absent paths to measure index/selection work without disk writes.
"""

import argparse
import json
import platform
import statistics
import subprocess
import tempfile
import time
import zipfile
from pathlib import Path


def positive_int(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/oxide")
    parser.add_argument("--baseline", type=Path, help="optional older release binary")
    parser.add_argument("--entries", type=positive_int, default=50_000)
    parser.add_argument("--selections", type=positive_int, default=1_000)
    parser.add_argument("--runs", type=positive_int, default=7)
    args = parser.parse_args()
    binaries = {"current": args.binary.resolve()}
    if args.baseline:
        binaries["baseline"] = args.baseline.resolve()
    for binary in binaries.values():
        if not binary.is_file():
            parser.error(f"binary does not exist: {binary}; build it with cargo build --release -p oxide-cli")

    report = {
        "platform": platform.platform(),
        "archive_entries": args.entries,
        "unmatched_selections": args.selections,
        "runs": args.runs,
        "benchmarks": {},
    }
    with tempfile.TemporaryDirectory(prefix="oxide-benchmark-") as temp:
        root = Path(temp)
        archive = root / "many.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as writer:
            for i in range(args.entries):
                writer.writestr(f"project/folder{i // 100}/file{i}.txt", b"test data")
        selection = []
        for i in range(args.selections):
            selection.extend(["--only", f"project/missing{i}"])
        operations = {
            "list": ["list", "-1", str(archive)],
            "selection_filter": ["extract", str(archive), "-o", str(root / "out"), *selection],
        }
        for name, operation in operations.items():
            samples = {label: [] for label in binaries}
            # Warm each binary, then alternate order to reduce cache/order bias.
            for run in range(args.runs + 1):
                labels = list(binaries)
                if run % 2:
                    labels.reverse()
                for label in labels:
                    started = time.perf_counter()
                    subprocess.run(
                        [str(binaries[label]), "--no-progress", "-q", *operation],
                        stdout=subprocess.DEVNULL,
                        check=True,
                    )
                    elapsed = time.perf_counter() - started
                    if run:
                        samples[label].append(elapsed)
            result = {
                label: {"median_seconds": statistics.median(times), "samples_seconds": times}
                for label, times in samples.items()
            }
            if "baseline" in result:
                result["speedup"] = result["baseline"]["median_seconds"] / result["current"]["median_seconds"]
            report["benchmarks"][name] = result
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
