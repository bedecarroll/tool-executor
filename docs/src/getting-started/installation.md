# Install

tx publishes as a standard Rust binary. Install from a local clone while the project is pre-release, or use `cargo install` once published.

```bash
# from a cloned repository
cargo install --path .

# once crates.io publishing begins
cargo install tool-executor
```

The build expects Rust 1.90 (edition 2024). If you use `rustup`, pin the toolchain by running `rustup override set 1.90.0` inside the repository. Tooling such as `cargo-nextest`, `cargo-insta`, and `cargo-llvm-cov` are managed through `mise`; run `mise trust` followed by `mise install` if you plan to execute the provided quality checks locally.

When packaging for a team, prefer `cargo dist` tasks (`mise run dist-*`) so CI and local builds stay aligned.
