#!/usr/bin/env python3
"""Compares two criterion baselines and fails if any benchmark's mean
estimate regressed by more than THRESHOLD (10%) from `base` to `pr`.
Reads criterion's own `estimates.json` per benchmark, under
target/criterion/<group>/<bench>/{base,pr}/estimates.json.
"""
import json
import sys
from pathlib import Path

THRESHOLD = 0.10
CRITERION_DIR = Path("target/criterion")


def mean_estimate(baseline_dir: Path) -> float | None:
    estimates_path = baseline_dir / "estimates.json"
    if not estimates_path.exists():
        return None
    with open(estimates_path) as f:
        data = json.load(f)
    return data["mean"]["point_estimate"]


def main() -> int:
    if not CRITERION_DIR.exists():
        print(f"no criterion output found at {CRITERION_DIR}, nothing to check")
        return 0

    regressions = []
    for bench_dir in sorted(CRITERION_DIR.glob("*/*/")):
        base_dir = bench_dir / "base"
        pr_dir = bench_dir / "pr"
        base_mean = mean_estimate(base_dir)
        pr_mean = mean_estimate(pr_dir)
        if base_mean is None or pr_mean is None:
            continue
        change = (pr_mean - base_mean) / base_mean
        label = f"{bench_dir.parent.name}/{bench_dir.name}"
        if change > THRESHOLD:
            regressions.append((label, change))
            print(f"REGRESSION  {label}: {change:+.1%} (base={base_mean:.0f}ns, pr={pr_mean:.0f}ns)")
        else:
            print(f"ok          {label}: {change:+.1%}")

    if regressions:
        print(f"\n{len(regressions)} benchmark(s) regressed by more than {THRESHOLD:.0%}")
        return 1
    print("\nno regressions over threshold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
