#!/usr/bin/env python3
"""Validate canonical application coverage, excluding separately compiled test sources."""
import argparse
import json
import math
from pathlib import Path
import re
import sys


METRICS = ("lines", "functions", "regions")


def fail(message):
    raise ValueError(message)


def source_inventory(root, policy):
    source = root / "crates/app/src"
    files = {p.relative_to(source).as_posix(): p for p in source.rglob("*.rs")}
    required = set(policy["pure_files"]) | set(policy["required_pure"]) | set(policy["io_files"]) | set(policy["declaration_only"])
    missing = required - files.keys()
    if missing:
        fail(f"missing canonical source modules: {sorted(missing)}")
    classified = {}
    required_pure = set(policy["pure_files"]) | set(policy["required_pure"])
    for name, path in files.items():
        if path.is_symlink():
            fail(f"source symlink not accepted: {name}")
        pure = name in required_pure or any(name.startswith(d + "/") for d in policy["pure_directories"])
        text = path.read_text()
        if re.search(r"#\s*\[\s*(?:test|bench)\s*\]|\bmod\s+tests\s*\{", text):
            fail(f"inline test code contaminates production coverage: {name}")
        for attribute in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", text):
            external = re.match(r'\s*#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]\s*mod\s+\w+\s*;', text[attribute.end():])
            if external is None:
                fail(f"production cfg(test) must introduce an external test module: {name}")
            target = (path.parent / external.group(1)).resolve()
            if not target.is_relative_to((root / "crates/app/tests").resolve()) or not target.is_file():
                fail(f"external test module escaped canonical tests: {name}")
        if name in required_pure and name in policy["declaration_only"]:
            fail(f"required pure module cannot be declaration-only: {name}")
        if name in policy["declaration_only"]:
            if re.search(r"\b(?:fn|impl)\b", path.read_text()):
                fail(f"declaration-only module acquired executable logic: {name}")
            classified[name] = "declaration"
        elif pure:
            classified[name] = "pure"
            if re.search(r"\b(?:std|crate)::(?:fs|process|thread|context|inventory|providers|operations|store|index_store|legacy|platform|catalog)\b|std::env::(?:var|var_os|vars|vars_os|set_var|remove_var|current_dir)\b|\buse\s+std::\{[^;]*\b(?:fs|process|thread|env)\b", text):
                fail(f"pure module depends on effects: {name}")
        elif name in policy["io_files"]:
            classified[name] = "io"
        else:
            fail(f"unclassified production module: {name}")
    return classified


def counters(summary, module):
    result = {}
    for metric in METRICS:
        value = summary[metric]
        count, covered, percent = value["count"], value["covered"], value["percent"]
        if type(count) is not int or type(covered) is not int or count < 0 or not 0 <= covered <= count:
            fail(f"invalid {metric} counters: {module}")
        expected = covered * 100 / count if count else 0
        if not isinstance(percent, (int, float)) or isinstance(percent, bool) or not math.isfinite(percent) or abs(percent - expected) > 0.000001:
            fail(f"inconsistent {metric} percentage: {module}")
        result[metric] = (covered, count)
    return result


def validate(root, report, policy):
    if policy.get("schema_version") != 1:
        fail("unsupported quality-policy schema")
    approved = {"lines": 90, "functions": 90, "regions": 85}
    if policy.get("pure") != {metric: 100 for metric in METRICS} or any(not approved[metric] <= policy["overall"][metric] <= 100 for metric in METRICS):
        fail("coverage policy is below the user-approved gates")
    classes = source_inventory(root, policy)
    data = report.get("data")
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0].get("files"), list):
        fail("coverage must contain one nonempty application export")
    source = (root / "crates/app/src").resolve()
    package = (root / "crates/app").resolve()
    measured = {}
    for record in data[0]["files"]:
        raw = Path(record["filename"])
        path = (raw if raw.is_absolute() else root / raw).resolve()
        if path.is_relative_to(package / "tests"):
            continue
        if not path.is_relative_to(source):
            fail(f"non-application source contaminates report: {path}")
        module = path.relative_to(source).as_posix()
        if module not in classes or module in measured:
            fail(f"unknown or duplicate source report: {module}")
        measured[module] = counters(record["summary"], module)
        if classes[module] != "declaration" and any(not count for _, count in measured[module].values()):
            fail(f"zero executable source counter: {module}")
    absent = {name for name, kind in classes.items() if kind != "declaration"} - measured.keys()
    if absent:
        fail(f"unmeasured application source: {sorted(absent)}")
    failures, overall = [], {metric: [0, 0] for metric in METRICS}
    pure_results = {}
    for name, values in measured.items():
        for metric, (covered, count) in values.items():
            overall[metric][0] += covered
            overall[metric][1] += count
        if classes[name] == "pure":
            percentages = {}
            for metric, (covered, count) in values.items():
                if not count:
                    fail(f"zero pure {metric} counter: {name}")
                percent = covered * 100 / count
                percentages[metric] = round(percent, 4)
                if percent < policy["pure"][metric]:
                    failures.append(f"pure {name} {metric}: {percent:.2f}% < {policy['pure'][metric]}%")
            pure_results[name] = percentages
    total = {}
    for metric, (covered, count) in overall.items():
        if not count:
            fail(f"zero application {metric} count")
        percent = covered * 100 / count
        total[metric] = round(percent, 4)
        if percent < policy["overall"][metric]:
            failures.append(f"overall {metric}: {percent:.2f}% < {policy['overall'][metric]}%")
    return {"overall": total, "pure": pure_results, "failures": failures, "production_modules": len(classes), "measured_modules": len(measured)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        result = validate(root, json.loads(args.report.read_text()), json.loads((root / "quality-policy.json").read_text()))
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        return bool(result["failures"])
    except (OSError, KeyError, TypeError, ValueError) as error:
        print(f"coverage_invalid: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
