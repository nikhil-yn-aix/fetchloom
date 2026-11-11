"""compare libtest selections with nextest and keep each filesystem's results."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "target/ci"
SKIP = ("source::", "net::", "glob::", "name::", "output::", "units::", "digest::",
        "error::", "basics::", "hints::", "progress::", "proxy::", "secrets::",
        "split::", "search::", "caps::")
CHMOD = "place::tests::a_writable_copy_works_where_the_mount_refuses_chmod"


def selected(document):
    return {f"{binary}::{name}"
            for binary, suite in document["rust-suites"].items()
            for name, case in suite["testcases"].items()
            if not case["ignored"] and case["filter-match"]["status"] == "matches"}


def require_same(expected, actual, scope):
    if not actual or expected != actual:
        missing, extra = sorted(expected - actual), sorted(actual - expected)
        raise RuntimeError(f"{scope}: {len(actual)} tests; missing {missing[:10]}, extra {extra[:10]}")


def output(command):
    return subprocess.run(command, cwd=ROOT, check=True, stdout=subprocess.PIPE,
                          text=True, encoding="utf-8").stdout


def options(args):
    if args.archive:
        return ["--archive-file", str(args.archive.resolve()), "--extract-to", str(ROOT),
                "--extract-overwrite", "--workspace-remap", str(ROOT)]
    return ["--locked", "--tests"]


def listing(args, filters=()):
    return json.loads(output(["cargo-nextest", "nextest", "list", "--profile", "ci",
                              "--message-format", "json", *options(args), *filters]))


def libtest(binary, filters=(), ignored=False):
    command = [str(binary), "--list", "--format", "terse", *filters]
    if ignored:
        command.append("--ignored")
    return {line.removesuffix(": test") for line in output(command).splitlines()
            if line.endswith(": test")}


def record(args):
    messages = output(["cargo", "test", "--locked", "--tests", "--no-run", "--message-format", "json"])
    binaries = {Path(item["executable"]).resolve()
                for line in messages.splitlines() if line.startswith("{")
                for item in [json.loads(line)]
                if item.get("reason") == "compiler-artifact" and item.get("executable")
                and item["profile"]["test"]}
    document = listing(args)
    suites = document["rust-suites"]
    listed = {Path(suite["binary-path"]).resolve() for suite in suites.values()}
    if binaries != listed:
        raise RuntimeError(f"cargo and nextest binaries differ: {binaries ^ listed}")
    expected = {scope: set() for scope in ("native", "disk", "chmod")}
    disk_flags = [word for skip in SKIP for word in ("--skip", skip)]
    for binary, suite in suites.items():
        path = suite["binary-path"]
        ignored = libtest(path, ignored=True)
        for scope, filters in (("native", ()), ("disk", disk_flags), ("chmod", (CHMOD, "--exact"))):
            expected[scope].update(f"{binary}::{name}" for name in libtest(path, filters) - ignored)
    require_same(expected["native"], selected(document), "native")
    RESULTS.mkdir(parents=True, exist_ok=True)
    (RESULTS / "selection.json").write_text(json.dumps({key: sorted(value) for key, value in expected.items()}, indent=2), encoding="utf-8")
    print("libtest selections:", {key: len(value) for key, value in expected.items()}, flush=True)


def run(args):
    scope = "disk" if args.disk else "chmod" if args.chmod else "native"
    expected = set(json.loads((RESULTS / "selection.json").read_text(encoding="utf-8"))[scope])
    filters = []
    if args.disk:
        filters = ["-E", "not (" + " | ".join(f"test({skip})" for skip in SKIP) + ")"]
    elif args.chmod:
        filters = ["-E", f"test(={CHMOD})"]
    document = listing(args, filters)
    actual = selected(document)
    require_same(expected, actual, args.scope)
    folder = RESULTS / args.scope
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "selected.json").write_text(json.dumps(sorted(actual), indent=2), encoding="utf-8")
    print(f"{args.scope}: {len(actual)} selected tests match cargo/libtest", flush=True)
    command = ["cargo-nextest", "nextest", "run", "--profile", "ci", "--no-tests", "fail",
               *options(args), *filters]
    env = dict(os.environ, INSTA_UPDATE="no", INSTA_FORCE_PASS="0", INSTA_WORKSPACE_ROOT=str(ROOT))
    references = folder / "snapshot-references.txt"
    references.write_text("", encoding="utf-8")
    env["INSTA_SNAPSHOT_REFERENCES_FILE"] = str(references)
    result = subprocess.run(command, cwd=ROOT, env=env, check=False)
    referenced = {Path(line.removeprefix("\\\\?\\")).resolve().relative_to(ROOT).as_posix()
                  for line in references.read_text(encoding="utf-8").splitlines() if line}
    (folder / "snapshots.json").write_text(json.dumps(sorted(referenced), indent=2), encoding="utf-8")
    references.unlink()
    junit = Path(document["rust-build-meta"]["target-directory"]) / "nextest/ci/junit.xml"
    if junit.is_file():
        shutil.copyfile(junit, folder / "junit.xml")
        tests = list(ET.parse(junit).iter("testcase"))
        if result.returncode == 0:
            executed = {f"{case.attrib['classname']}::{case.attrib['name']}" for case in tests}
            require_same(actual, executed, f"{args.scope} executed")
        slow = sorted(tests, key=lambda case: float(case.get("time", "0")), reverse=True)[:10]
        summary = f"### {args.scope}: {len(actual)} tests match libtest\n\n"
        summary += "| test | seconds |\n|---|---:|\n"
        summary += "".join(f"| {case.get('name')} | {case.get('time')} |\n" for case in slow)
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as report:
                report.write(summary)
    elif result.returncode == 0:
        raise RuntimeError("a successful run produced no JUnit report")
    subprocess.run(["git", "diff", "--exit-code", "--", "Cargo.lock"], cwd=ROOT, check=True)
    return result.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("record", "run"))
    parser.add_argument("scope", nargs="?", default="native")
    parser.add_argument("--disk", action="store_true")
    parser.add_argument("--chmod", action="store_true")
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    if args.command == "record":
        record(args)
        return 0
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
