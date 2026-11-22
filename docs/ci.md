# CIとローカル検証

`.github/workflows/ci.yml` はすべてのPR、mainへのpush、手動実行で `macOS checks` を実行する。`macos-15` のネイティブ環境で、いずれかのコマンドが失敗するとジョブも失敗し、PRのChecksに結果が表示される。branch protectionの必須チェック設定はこの変更の対象外。

```bash
cargo fmt --all --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

Rustは `rust-toolchain.toml` に固定された版を使う。CIで `rustup show` がこのファイルに従ってツールチェーンを導入し、rustfmtとClippyも追加する。依存はコミット済みの `Cargo.lock` を使い、更新が必要なら `--locked` が失敗する。最後にlockfileが変更されていないことも確認する。

Cargoのregistry・git依存・ビルド成果物は `Swatinem/rust-cache` でキャッシュする。キーにはOS、Rust、Cargo.lockなどが含まれる。PRはmainのキャッシュを復元し、mainの成功時にキャッシュを保存する。同じPR/ブランチの新しい実行は古い実行を取り消す。

## テスト環境

開発MacにはXcode Command Line Tools、rustup、PATH上の `python3` が必要。CIは `actions/setup-python` でPython 3.12を導入する。Pythonの追加パッケージは不要。実PTY fixtureは標準ライブラリの `os`、`select`、`termios`、`tty` を使うため、端末作成と子プロセス起動を許可するUnix環境で実行する。

通常の `cargo test` は単体テスト、LSPのfake server、実PTYの問い合わせ・入力モード・終了状態のテストを含む。GUI操作や日本語IMEの候補選択は自動化していない。実機確認を別に実施する。

実rust-analyzerを使う `lsp::tests::rust_analyzer_definition` はignoredとして通常のCIから除外する。サーバー起動とindex完了が必要なため、Rustプロジェクトの変更時やリリース前に次を実行する。

```bash
rustup component add rust-analyzer
cargo test --locked lsp::tests::rust_analyzer_definition -- --ignored --nocapture
```

TypeScript language serverと実エージェントCLIは通常のCIでは起動しない。使う機能に応じてサーバー・CLIを導入した実機で確認する。

## ビルド確認の範囲

PRのCIは `macos-15` ランナーのネイティブビルドを確認する。Intel Macへのクロスビルド、Linux x86_64のビルドとGUI動作は、このCIの成功だけでは確認できない。

既存の `release.yml` はmacOS arm64、macOS Intelのクロスビルド、Linux x86_64を対象にする。タグpushと手動実行用であり、PRチェックには使わない。配布再開時は各対象のreleaseビルドと起動を別に確認する。CIの追加ではタグ作成、リリース公開、Homebrew tap公開は実行しない。
