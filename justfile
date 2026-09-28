[unix]
set shell := ["bash", "-euo", "pipefail", "-c"]
[windows]
set shell := ["C:/Program Files/Git/bin/bash.exe", "-euo", "pipefail", "-c"]
[unix]
set script-interpreter := ["bash", "-euo", "pipefail"]
[windows]
set script-interpreter := ["C:/Program Files/Git/bin/bash.exe", "-euo", "pipefail"]

tools := "just@1.58.0,ripgrep@15.2.0,cargo-nextest@0.9.146,cargo-llvm-cov@0.9.1,cargo-deny@0.20.2,cargo-machete@0.9.2,cargo-mutants@27.1.0,hyperfine@1.20.0"
local_tools := "samply@0.13.1,cargo-fuzz@0.13.2"

self := quote(just_executable()) + " --justfile " + quote(justfile())

exe := if os_family() == "windows" { ".exe" } else { "" }
fl_release := "target/release/fl" + exe
coverage_gates := "fl-model=85 fl-store=85 fl-transfer=85 fl-archive=85 fl-steps=85 fl-sources=75 fl-cli=75"

em_dash :='\x{2014}'
rust_comment := '(^|[^:/"])//([^/!]|$)|(^|\s)/\*|(^|\s)////'
python_comment := '^\s*#([^!]|$)|\s#'

[doc("list the recipes")]
default:
    @{{self}} --list

[doc("install the pinned tools with cargo-binstall")]
tools:
    cargo binstall --no-confirm --locked --disable-telemetry {{replace(tools, ",", " ")}} {{replace(local_tools, ",", " ")}}

[doc("check formatting")]
fmt:
    cargo fmt --all --check

[doc("clippy on every target, warnings denied")]
clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

[doc("no em dashes anywhere, no comments in rust or python")]
text dir=".":
    @{{self}} _forbid "em dash" '{{em_dash}}' '{{dir}}'
    @{{self}} _forbid "rust comment" '{{rust_comment}}' '{{dir}}' rust
    @{{self}} _forbid "python comment" '{{python_comment}}' '{{dir}}' py

[private]
_forbid rule pattern dir type="":
    @rg --hidden --glob '!.git' --line-number {{ if type == "" { "" } else { "--type " + type } }} -e '{{pattern}}' '{{dir}}' && { echo "text rule broken: {{rule}}" >&2; exit 1; } || [ $? -eq 1 ]

[doc("prove every text rule fails on bad lines and passes good ones")]
[script]
text-selftest:
    root=$(mktemp -d)
    trap 'rm -rf "$root"' EXIT
    rule() { {{self}} text "$1"; }
    mkdir "$root/good"
    printf '%s\n' '//! crate doc' '/// item doc' 'let url = "https://example.com";' 'let glob = "**/*.edf";' 'let slashes = "//";' > "$root/good/a.rs"
    printf '%s\n' '#!/usr/bin/env python' '"""Doc."""' 'x = "#not"' > "$root/good/a.py"
    printf 'plain - hyphen\n' > "$root/good/a.txt"
    mkdir "$root/good/skipped"
    printf 'skipped/\n' > "$root/good/.ignore"
    printf 'ignored %s file\n' "$(printf '\xe2\x80\x94')" > "$root/good/skipped/a.txt"
    rule "$root/good" || { echo "text rules refused good input" >&2; exit 1; }
    n=0
    bad() {
        n=$((n + 1))
        mkdir "$root/bad$n"
        printf '%s\n' "$2" > "$root/bad$n/$1"
        if rule "$root/bad$n" >/dev/null 2>&1; then
            echo "text rules missed in $1: $2" >&2
            exit 1
        fi
    }
    bad a.txt "a $(printf '\xe2\x80\x94') b"
    bad a.rs '// line'
    bad a.rs 'let x = 1; // trailing'
    bad a.rs '    //indented'
    bad a.rs '//'
    bad a.rs '////'
    bad a.rs '/* block */'
    bad a.rs 'let y = 2; /* block */'
    bad a.py '# line'
    bad a.py 'x = 1  # trailing'
    bad a.py '    # indented'
    bad a.py 'y = 2  # noqa: E501'
    bad a.py '#'
    echo "text rules: $n bad inputs refused, good input accepted"

[doc("licenses, bans and sources of every dependency")]
deny:
    cargo deny --locked check bans licenses sources

[doc("security advisories for every dependency")]
advisories:
    cargo deny --locked check advisories

[doc("no unused dependencies")]
machete:
    cargo machete

[doc("every test, every target, no early stop")]
test:
    cargo nextest run --workspace --locked --no-fail-fast

[doc("live provider tests, the ignored ones, against the real network")]
live:
    FETCHLOOM_LIVE=1 cargo nextest run --workspace --locked --no-fail-fast --run-ignored only --no-tests=warn

[doc("line coverage of the test suite, gated per crate")]
cov:
    cargo llvm-cov nextest --workspace --locked --no-fail-fast --no-report
    cargo llvm-cov report
    {{self}} _coverage-gate

[private]
[script("uv", "run", "--no-project", "--quiet", "python")]
_coverage-gate:
    import json
    import subprocess
    import sys

    below = []
    for gate in "{{coverage_gates}}".split():
        crate, minimum = gate.split("=")
        report = subprocess.run(
            ["cargo", "llvm-cov", "report", "--json", "--summary-only", "--package", crate],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        lines = json.loads(report)["data"][0]["totals"]["lines"]
        if lines["count"] == 0:
            print(f"{crate}: no code yet")
            continue
        passed = lines["percent"] >= float(minimum)
        verdict = "ok" if passed else "below the gate"
        print(f"{crate}: {lines['percent']:.2f}% of {lines['count']} lines, gate {minimum}%, {verdict}")
        if not passed:
            below.append(crate)
    sys.exit(1 if below else 0)

[doc("mutation testing of one crate")]
mutants crate:
    cargo mutants --package {{crate}} --test-tool nextest --output target

[doc("mutation testing of the whole workspace")]
mutants-all:
    cargo mutants --workspace --test-tool nextest --output target

[doc("run one fuzz target for a number of seconds")]
fuzz target seconds="600":
    cargo +nightly fuzz run {{target}} -- -max_total_time={{seconds}}

[doc("run every fuzz target for a number of seconds each")]
[script]
fuzz-all seconds="600":
    if [ ! -d fuzz ]; then
        echo "no fuzz targets yet"
        exit 0
    fi
    rustup toolchain install nightly --profile minimal
    for target in $(cargo +nightly fuzz list); do
        {{self}} fuzz "$target" {{seconds}}
    done

[doc("benches and command timings on a release build")]
bench:
    cargo bench --workspace --locked --benches
    cargo build --release --locked --package fl-cli
    hyperfine --shell=none --warmup 20 --runs 200 '{{fl_release}} --version'
    echo "fl binary: $(wc -c < {{fl_release}}) bytes"

[doc("a samply profile of fl with the given arguments")]
profile *args:
    cargo build --profile profiling --locked --package fl-cli
    samply record --save-only --output target/profile.json.gz target/profiling/fl{{exe}} {{args}}
    echo "view it with: samply load target/profile.json.gz"

[doc("every static check")]
lint: fmt clippy text text-selftest deny machete

[doc("lint and test")]
check: lint test

[doc("everything CI runs on a pull request")]
ci: lint test cov
