what changed and why:

how it was tested (the narrowest test that covers it, then the gate):

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo test --locked`
- [ ] the owning page under `docs/` is updated if behavior changed

see [CONTRIBUTING.md](../CONTRIBUTING.md) and [style](../docs/dev/style.md) for commits and prose.
