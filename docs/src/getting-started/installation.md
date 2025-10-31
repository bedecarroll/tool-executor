# Install

tx publishes as a standard Rust binary. Install from the GitHub releases if you want a ready-to-run build, or compile from source when you need to hack locally.

## Install from a release

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/bedecarroll/tool-executor/releases/latest/download/tx-installer.sh \
  | sh
```

Windows users can run:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/bedecarroll/tool-executor/releases/latest/download/tx-installer.ps1 | iex"
```

The scripts install `tx` into your Cargo binary directory and verify checksums. Manual downloads are available on the [release page](https://github.com/bedecarroll/tool-executor/releases/latest)—look for the `tx-<target>.tar.gz` (Unix) or `tx-<target>.zip` (Windows) assets if you prefer to unpack them yourself.

## Build from source

```bash
# from a cloned repository
cargo install --path .

# once crates.io publishing begins
cargo install tool-executor
```

The build expects Rust 1.90 (edition 2024). If you use `rustup`, pin the toolchain by running `rustup override set 1.90.0` inside the repository. Tooling such as `cargo-nextest`, `cargo-insta`, and `cargo-llvm-cov` are managed through `mise`; run `mise trust` followed by `mise install` if you plan to execute the provided quality checks locally.

When packaging for a team, prefer `cargo dist` tasks (`mise run dist-*`) so CI and local builds stay aligned.
