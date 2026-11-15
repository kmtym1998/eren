# eren

Rust + GPUI（gpui-kit 経由）で作る、コーディングエージェント用のネイティブワークスペース。
プロジェクトとエージェントのサイドバー、LSP ジャンプ付きの読み取り専用コードビューア、Git 差分、ターミナルを順に実装する。
現状は 3 ペインの UI 骨格のみ。計画と決定事項は [SPEC.md](SPEC.md) を参照。

## Mac での開発

macOS 15 以降と Xcode Command Line Tools が必要。Xcode を導入済みならそのまま使える。

```bash
# Xcode / Command Line Tools が未導入の場合
xcode-select --install

# Rust が未導入の場合（公式 rustup）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/eren-rustup-init.sh
sh /tmp/eren-rustup-init.sh -y --profile default --component rust-analyzer
source "$HOME/.cargo/env"
```

リポジトリのルートで実行する。初回は依存クレートの取得とコンパイルに時間がかかる。

```bash
cargo run --locked
```

`rust-toolchain.toml` で Rust のバージョンと開発用コンポーネントを指定する。
`Cargo.lock` の依存バージョンを使うため、通常は `--locked` を付ける。

```bash
cargo check --locked
cargo fmt --check
cargo clippy --locked -- -D warnings
```

GUI を開かずにバージョンやヘルプを確認するには次を使う。

```bash
cargo run --locked -- --version
cargo run --locked -- --help
```

## Homebrew での配布（予定）

初回リリースとリポジトリの公開後は、次のコマンドで導入する想定。

```bash
brew install kmtym1998/tap/eren
eren
```

`v*` タグを push すると GitHub Actions が macOS arm64 / Intel、Linux x86_64 用のリリースをビルドする。
Formula の雛形は `dist/homebrew/eren.rb` にある。
