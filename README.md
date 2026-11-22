# eren

RustとGPUI（gpui-kit経由）で作る、コーディングエージェント用のネイティブワークスペース。ターミナルを中心に、ファイル編集、LSPジャンプ、Git差分を同じウィンドウで使う。

## 使える機能

- プロジェクトのファイルツリー、worktree一覧と切り替え。フォルダ選択とCLI引数の両方で開ける。
- 複数のPTYセッション。シェル、Claude Code、Codex、OpenCodeを起動できる。各CLIは別途PATHへ導入する。エージェントの状態表示は画面の文字列からの推定で、誤判定する場合がある。
- UTF-8ファイルの編集・保存、構文ハイライト、未保存の変更を保護する切り替え確認。外部変更の検知、保存競合の確認、一時ファイルを使う原子的な保存に対応する。
- Gitルート基準のChanges一覧と統合・左右分割の差分表示。サブフォルダから開いた場合、初回コミット前の差分、リネームにも対応する。
- `.gitignore`を尊重するプロジェクト内検索。大文字小文字を区別しない部分一致で、結果から該当位置へ移動する。検索は1行1件、最大300件で、2 MB超とバイナリを除く。
- RustとTypeScript/JavaScriptのLSP。定義・実装・参照、hover、診断、シンボル一覧、戻る・進む履歴に対応する。
- 端末の履歴10,000行、マウス選択・コピー、貼り付け、URL/ファイルリンクのCmd+クリック、日本語IME。全角・結合文字、ANSI/256色/truecolor、端末からの問い合わせ、application cursorとbracketed pasteを扱う。
- シェル終了時の終了コード表示と再起動・閉じる操作。追加起動の失敗は既存の端末を隠さず通知する。
- プロジェクト、セッション構成、開いたファイル、エディタ表示状態を `~/.local/state/eren/state.json` に保存して復元する。端末のプロセスや出力履歴は引き継がず、起動し直す。

仕様と未実装範囲は [SPEC.md](SPEC.md) を参照。

## Macでの開発

macOS 15以降、Xcode Command Line Tools、rustup、PATH上のPython 3が必要。実PTYとLSPのfixtureテストは `python3` の標準ライブラリを使う。Pythonの追加パッケージは不要。

```bash
# Command Line Toolsが未導入の場合
xcode-select --install

# rustupが未導入の場合
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/eren-rustup-init.sh
sh /tmp/eren-rustup-init.sh -y --profile default --component rust-analyzer
source "$HOME/.cargo/env"
```

リポジトリのルートで実行する。`rust-toolchain.toml` の固定Rustを使い、依存は `Cargo.lock` に従う。初回の依存取得とビルドには時間がかかる。

```bash
cargo run --locked -- /path/to/project
```

引数なしでは保存済みの構成を復元する。GUIを開かない確認もできる。

```bash
cargo run --locked -- --version
cargo run --locked -- --help
```

変更後は次を実行する。PRとmainへのpushでもMacのCIが同じ確認を実行する。確認範囲と実サーバーのテストは [docs/ci.md](docs/ci.md) を参照。

```bash
cargo fmt --all --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

実rust-analyzerの結合テストは通常実行ではignoredになる。サーバーの起動とindex完了を含めて確認する場合に実行する。

```bash
rustup component add rust-analyzer
cargo test --locked lsp::tests::rust_analyzer_definition -- --ignored --nocapture
```

Rustプロジェクトではrust-analyzerを使う。TypeScript/JavaScriptではPATH上の `typescript-language-server` と、プロジェクト側の `typescript` が必要。`Cargo.toml` と `package.json` が両方ある場合はRustを優先する。補完、rename、LSPによる整形には未対応。

## 主な操作

| 操作 | キー・操作方法 |
| --- | --- |
| 保存 | Cmd+S / Ctrl+S |
| エディタの表示切り替え | Cmd+E / Ctrl+E |
| 定義・実装・参照 | F12 / Ctrl+F12 / Shift+F12 |
| シンボル一覧 | Cmd+Shift+O / Ctrl+Shift+O |
| 戻る・進む | Alt+左 / Alt+右 |
| 端末のコピー | 選択してCmd+C / Ctrl+Shift+C |
| 端末の貼り付け | Cmd+V / Ctrl+Shift+V |
| 端末の履歴 | ホイール / Shift+PageUp・PageDown |
| 端末のリンク | Cmd+クリック |

## 既知の制約と確認状況

検索・Git・外部変更検知にはUIスレッド上の同期I/Oが残り、大きなプロジェクトで表示が止まる場合がある（[#18](https://github.com/kmtym1998/eren/issues/18)）。linked worktreeやサブフォルダのブランチ名表示（[#48](https://github.com/kmtym1998/eren/issues/48)）、末尾改行なしの分割差分（[#31](https://github.com/kmtym1998/eren/issues/31)）、空白を含む引用付きファイルリンク（[#32](https://github.com/kmtym1998/eren/issues/32)）、改行を含むworktreeパス（[#33](https://github.com/kmtym1998/eren/issues/33)）は未解決。[Issue一覧](https://github.com/kmtym1998/eren/issues?q=is%3Aissue+is%3Aopen) で修正状況を確認できる。

Macでは日本語の確定、Ctrl+Shift+Jで余分な改行を送らないこと、日本語のコピー、全角・結合文字・絵文字、16色、ブロック・下線・バーのカーソルをGUIで確認した。IMEの取消・再変換・変換中の全セッション操作を組み合わせた実機確認は未完了。これらの確定先の規則は回帰テストで検証している。

終了コードはPTYの直接の子プロセスの値を表示する。シェル設定がKiroなどのラッパーを起動すると、内側のシェルとラッパーの終了コードが異なる場合がある。設定を隔離したzshではGUIでも終了コード7と再起動を確認した。

LinuxとIntel Macには配布用ビルド設定があり、GUI操作には未検証の範囲がある。Windowsは対象外。アプリ終了後もプロセスを維持する常駐ホストは未実装。

## 配布状況

既存の [v0.1.0リリース](https://github.com/kmtym1998/eren/releases/tag/v0.1.0) にはmacOS arm64・Intel、Linux x86_64の成果物がある。現在のmainに入った修正は、この既存リリースには含まれない。

Homebrew tapの公開・導入確認は保留中。導入コマンドは配布再開時に確認する。再開手順と既存成果物の情報は [HOMEBREW.md](HOMEBREW.md)、Formulaの雛形は `dist/homebrew/eren.rb` にある。

`release.yml` は `v*` タグで配布物を作成・公開する設定で、手動実行ではActionsの成果物を作る。新規リリースとtap公開は、対象ごとのビルド・起動を確認してから別途判断する。配布用の公証も未対応。
