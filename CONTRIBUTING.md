# contributing

a change goes like this:

1. set up and run one test, as the [developer guide](docs/dev/README.md#start-here) shows. the [code map](docs/dev/architecture/codemap.md) finds the file that owns what you want to change.
2. write a test that fails on the current code, at the lowest layer that shows the behavior. make the change, then run the full gate the [engineering loop](docs/dev/engineering/README.md#the-fast-loop) lists.
3. update the user or developer page that owns the behavior in the same change. a speed or memory claim needs a measurement, as [running benchmarks](docs/dev/performance/README.md#running-benchmarks) describes.
4. keep the boundaries [decisions](docs/dev/decisions/README.md) records, or say in the pull request why one should move.
5. write prose, comments and commit messages as [style](docs/dev/style.md) says, and fill in the pull request template's checklist.

everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md). report a security weakness privately, as [security](SECURITY.md) describes, never in a public issue.
