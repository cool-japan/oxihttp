# Contributing to OxiHTTP

OxiHTTP is part of the **COOLJAPAN ecosystem**, a family of Pure-Rust crates
maintained by COOLJAPAN OU (Team Kitasan). Thank you for your interest in
contributing.

## Building and Testing

This is a Cargo workspace with four member crates: `oxihttp-core`,
`oxihttp-client`, `oxihttp-server`, and the `oxihttp` facade.

```sh
# Run the full test suite (uses cargo-nextest)
cargo nextest run

# Run doctests (nextest does not run these)
cargo test --doc

# Lint with zero tolerance for warnings
cargo clippy --all-targets -- -D warnings

# Check a specific feature combination, e.g. static file serving
cargo nextest run --workspace --features oxihttp-server/static-files
```

A change is not considered done until both `cargo nextest run` and
`cargo clippy --all-targets -- -D warnings` are clean with default features.

## MSRV

The default feature set requires **Rust 1.80+** (`rust-version` in the workspace
`Cargo.toml`). This is a floor for the default set only: `tls`, `h3`, and
`compression`/`decompression` each pull in a sibling COOLJAPAN crate with its own,
higher declared `rust-version` (1.89, 1.85, and 1.85 respectively, as actually
published to crates.io — verify with `cargo tree --features <feature>` against a
clean `Cargo.lock`, not just the sibling repo's current checkout, which can be ahead
of what it last published). `h3`'s floor moved above the default as of the
`oxiquic-h3`/`oxiquic-crypto` 0.2.1 bump (0.2.0 published `rust-version = "1.80"`;
0.2.1 publishes `rust-version = "1.85"`). `cargo`'s resolver enforces each floor
automatically once the corresponding feature is enabled. See the MSRV table in
README.md for the full breakdown. If you bump one of those optional dependencies,
check whether its *published* `rust-version` moved and update the README table in
the same change.

## Project Rules

- **Pure Rust by default.** No C/C++/Fortran dependency, and no non-default
  C feature, in the default feature set. If a feature genuinely requires a
  native dependency, it must be opt-in and clearly documented.
- **No `unwrap()`/`expect()`/`panic!` on untrusted input.** Any data derived
  from the network, a file, or a caller must be handled with the crate's
  typed [`OxiHttpError`](crates/oxihttp-core/src/error.rs) — see that
  module's "`BoxError` bounds policy" doc comment for the one narrow
  exception (foreign trait bounds at an internal adapter boundary).
- **No compiler or clippy warnings.** `cargo clippy --all-targets -- -D
  warnings` must pass cleanly before a change is merged.
- **Workspace dependency inheritance.** Add new dependencies to
  `[workspace.dependencies]` in the root `Cargo.toml` and reference them
  from member crates with `dep.workspace = true`; do not pin ad-hoc
  versions in a member `Cargo.toml`.
- **Keep files under 2000 lines.** Split large modules rather than letting
  a single file grow unbounded.
- **Match existing style.** `snake_case` naming, `///` doc comments with
  runnable examples on key public items, and comment density consistent
  with the surrounding code.
- **Never hardcode absolute paths** in code, tests, or docs — use
  `std::env::temp_dir()` and relative/workspace-relative paths instead.

## Submitting Changes

1. Open an issue or discussion for anything beyond a small fix, so the
   approach can be agreed on first.
2. Keep pull requests focused; unrelated changes should be split out.
3. Add or update tests for behavioral changes, including property/fuzz
   tests for anything that parses untrusted bytes.
4. Ensure `cargo nextest run` and `cargo clippy --all-targets -- -D
   warnings` are both clean before requesting review.

For security vulnerabilities, please follow [SECURITY.md](SECURITY.md)
instead of opening a public issue or pull request.
