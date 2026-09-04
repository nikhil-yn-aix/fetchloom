# Contributing

## What the machine does to the numbers

Every timing number in this repository is a property of the machine that produced
it. On the Windows development machine these two effects are large enough to
swamp any change to the code.

Real-time antivirus scans every file the suites write. During one suite run
`MsMpEng.exe` grew from 3.0 GB to 5.5 GB on a machine with 15.6 GB of memory. In
that state the many-small-files regime reported small writes costing 47, 49, 57
and 173 times one large write across four runs of identical code. Add exclusions
for the workspace directory and for `%TEMP%` before any timing number measured
here means anything. Without them the numbers measure the scanner.

Memory pressure decides how the suites are run. Three background
`cargo test --workspace` runs were killed for low memory. Suites are therefore run
one crate at a time, in the foreground, with `--test-threads=2`.
