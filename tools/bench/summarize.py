#!/usr/bin/env python3
"""Markdown tables from phase.sh output: medians over runs, with the range.

Usage: summarize.py results/bridges.txt results/hosts.txt
Labels are STACK-RUN[-part]; sample.sh lines and bench.py JSON lines mix.
"""

import json
import re
import statistics
import sys
from collections import defaultdict

SAMPLE = re.compile(r"^(\w+)-(\d+)-(idle|work) pid=(\d+) comm=(\S+) (.*)")
PROCS = re.compile(r"^\w+-\d+-procs (\S+) (\d+) ")
FIELDS = ["pss_kb", "uss_kb", "private_dirty_kb", "rss_kb", "threads",
          "cpu_ms", "vol_s", "slices_s"]


def spread(values):
    values = [v for v in values if v is not None]
    if not values:
        return "n/a"
    middle = statistics.median(values)
    if len(values) == 1:
        return f"{middle:g}"
    return f"{middle:g} ({min(values):g}–{max(values):g})"


def main():
    runs = defaultdict(list)
    pooled = defaultdict(list)
    units = {}
    for path in sys.argv[1:]:
        for line in open(path):
            line = line.strip()
            procs = PROCS.match(line)
            if procs:
                units[procs[2]] = procs[1]
            match = SAMPLE.match(line)
            if match:
                stack, _, part, pid, comm, rest = match.groups()
                comm = units.get(pid, comm)
                values = dict(field.split("=") for field in rest.split())
                for field in FIELDS:
                    runs[(stack, comm, f"{part} {field}")].append(float(values[field]))
                if part == "work":
                    window = float(values["window_s"])
                    runs[(stack, comm, "work cpu_ms_per_s")].append(
                        round(float(values["cpu_ms"]) / window, 2))
                continue
            if not line.startswith("{"):
                continue
            result = json.loads(line)
            stack = result["label"].split("-")[0]
            part = result["label"].split("-")[2] if result["label"].count("-") > 1 else ""
            if result["kind"] == "startup":
                for key in ("name_ms", "peers_ms", "boot_ms"):
                    if result[key] is not None:
                        runs[(stack, result["name"], f"startup {part} {key}".replace("  ", " "))].append(result[key])
            else:
                key = (stack, result["kind"], "latency ms")
                runs[key].append(result.get("median_ms"))
                pooled[key] += result["samples_ms"]
                runs[(stack, result["kind"], "missing")].append(result["missing"])
    print("| Stack | Process / probe | Metric | Median over runs (min–max) | Runs |")
    print("|---|---|---|---:|---:|")
    for (stack, what, metric), values in sorted(runs.items()):
        print(f"| {stack} | {what} | {metric} | {spread(values)} | {len(values)} |")
    print()
    print("| Stack | Probe | Samples | Median ms | p90 ms | Max ms |")
    print("|---|---|---:|---:|---:|---:|")
    for (stack, kind, _), samples in sorted(pooled.items()):
        if samples:
            ordered = sorted(samples)
            p90 = ordered[min(len(ordered) - 1, int(len(ordered) * 0.9))]
            print(f"| {stack} | {kind} | {len(ordered)} | {statistics.median(ordered):g} | {p90:g} | {ordered[-1]:g} |")


if __name__ == "__main__":
    main()
