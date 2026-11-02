//! the `fetchloom` command.

#[cfg(all(target_os = "linux", target_env = "musl"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[expect(
    clippy::print_stderr,
    reason = "a crashing program has nothing but stderr left to say it on"
)]
fn main() -> std::process::ExitCode {
    // a panic is fetchloom's own bug: say so and where to report it, before rust's own words
    let rust = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "fetchloom stopped on a bug of its own, not anything you did. please report it at \
             https://github.com/nikhil-yn-aix/fetchloom/issues with the command you ran and the \
             output of `fetchloom --version -v`. what it said:"
        );
        rust(info);
    }));
    fetchloom::main()
}
