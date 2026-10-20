//! What the macros accept and what they refuse, with the error and the span
//! the compiler shows. The `.stderr` files are the output of the rustc pinned
//! in `mise.toml`; `TRYBUILD=overwrite cargo test -p openshop --test macros`
//! rewrites them. They hold the errors of the macros, which the compiler
//! prints the same from one version to the next, not the ones it makes up.

#[test]
fn macros() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/macros/pass/*.rs");
    tests.compile_fail("tests/macros/fail/*.rs");
}
