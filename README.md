# eren

A Sirio/Herdr-like native workspace for coding agents, built from scratch
with Rust + GPUI (via gpui-kit): project/agent sidebar, a read-only code
viewer with LSP jumps (definition / implementation / references), git diffs,
and — later — terminal panes where the agents live.

See [SPEC.md](SPEC.md) for the plan and locked decisions.

```bash
cargo run
```

## Install via Homebrew (planned)

Once the first release is out and this repository is public:

```bash
brew install kmtym1998/tap/eren
eren
```

Releases are built by GitHub Actions from `v*` tags (macOS arm64 / Intel,
Linux x86_64). The formula template lives in `dist/homebrew/eren.rb`.
