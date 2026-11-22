# eren 仕様メモ

2026-11-22時点の実装と残作業。Rust、GPUI、gpui-kitで作るネイティブワークスペース。リポジトリは [kmtym1998/eren](https://github.com/kmtym1998/eren)（public）。

## UIと構成

ターミナルを中央ペインの主役にする。左にプロジェクト・worktree・セッション、右にGit Changesを置く。エディタはファイルを開くかトグル操作したときに表示し、端末と並べる。差分表示と通常編集を切り替えても未保存の文書を保持する。

本体は `gpui-kit` 経由でGPUIを使う。GPUIのテスト支援だけは、kitと同じ `gpui-pre` の固定版をdev-dependencyに持つ。ターミナルは `alacritty_terminal` がPTYとVTグリッドを管理し、GPUI側で描画・入力を扱う。

## 実装済み

### プロジェクト・編集・検索

- フォルダ選択またはCLI引数でプロジェクトを開く。ファイルツリー、ブランチ名、Git worktree一覧を表示し、worktreeを切り替えられる。
- UTF-8のファイルを編集・保存する。不正UTF-8、NULを含むファイル、2 MiB超はエディタで開かない。構文ハイライトはRust、TypeScript/TSX、JavaScript、TOML、Markdown、CSS、HTML、Bash、YAML、Python、JSONに対応する。
- 未保存のままファイルや履歴上の別の文書へ移動する場合は、保存・破棄・キャンセルを選ぶ。読み込みや保存に失敗した場合も、現在の文書を保持する。
- 約1秒ごとに外部変更を検知する。未編集ならカーソル位置を保って読み直し、未保存なら警告する。保存前にはディスク内容と比較して競合を確認する。一時ファイルへの書き込み後に置き換え、シンボリックリンクと権限を保持する。
- `.gitignore`を尊重する横断検索。小文字化による部分一致で、Unicodeの検索位置を元の列へ戻す。1行1件、上限300件。2,000,000 bytes超とバイナリを検索対象から除く。
- Gitルート基準のChanges一覧と統合・左右分割の差分表示。サブフォルダから開いてもリポジトリ全体を基準に扱い、初回コミット前のstaged差分、未追跡、Gitが検出したリネーム・コピーの元パスを含む差分も表示する。Gitの失敗は差分なしと区別して通知する。

### LSP

stdioのJSON-RPCクライアントを使う。読み取り専用スレッドとバックグラウンド要求でUIへ結果を返す。プロジェクト、文書、要求の版を確認し、古い結果が別の表示へ混ざらないようにする。切断・終了・起動取消時には要求と子プロセスを解放する。

Rustプロジェクトではrust-analyzerを自動起動し、rustupのshimよりtoolchain内の実体を優先する。`package.json`のあるプロジェクトではPATH上の `typescript-language-server --stdio` を使う。TypeScript本体はプロジェクト側に必要。両方の設定ファイルがある場合はRustを優先する。

対応するUIは定義、実装、参照、hover、診断、documentSymbol一覧、戻る・進む履歴。サーバーのcapabilityを確認して要求し、対応言語の文書をopen/change/closeで同期する。変更同期は全文で、UTF-16の位置を変換する。hoverは文字列・言語付き内容・配列・MarkupContentを扱う。`$/progress`をステータスバーへ表示する。

### ターミナル・エージェント

プロジェクト内でログインシェルを起動する。複数セッションを持ち、Claude Code、Codex、OpenCodeも起動できる。各CLIはPATH上に必要。エージェントのコマンドが終了するとシェルへ移る。

- ANSI16色、256色、truecolor、背景、反転、OSC色変更・リセットと問い合わせ応答。
- 全角・結合文字、行末折り返し、セル幅に対応したカーソル・選択・コピー・リンク位置。
- 履歴10,000行。ホイールまたはShift+PageUp/PageDownで移動し、通常入力で最下部へ戻る。端末へのプロトコル応答では履歴位置を変えない。
- 選択したテキストのコピー、貼り付け、URLとファイルパスのCmd+クリック。APP_CURSORとBRACKETED_PASTEに応じて入力を符号化する。
- IMEのmarked textをカーソル位置へ表示し、確定時にだけ送信する。変換開始時のセッションIDを保持し、切り替え・追加後も元の端末へ確定する。元の端末を閉じた場合は捨てる。Ctrl+Shift+Jは入力ソース切り替えに渡し、Ctrl+J単独は維持する。
- シェル終了コードと終了状態を表示し、入力を止め、再起動・閉じる操作を用意する。起動失敗の通知は既存端末の表示と分ける。

エージェントの「作業中」「入力待ち」「不明」は可視テキストからの推定。CLIの表示変更や画面に残った出力で誤判定する場合がある。シェル自体の終了状態は、推定とは別にPTYの子の終了通知から得る。

### 構成の復元

`~/.local/state/eren/state.json` にプロジェクト、セッション構成、アクティブセッション、開いたファイル、エディタ表示状態を保存する。引数なし起動で復元し、復元途中の状態で保存内容を上書きしない。保存は原子的に置き換える。端末のプロセスと出力履歴は復元せず、プロセスは起動し直す。

## 確認済みの範囲と残作業

単体・fake LSP server・実PTYの回帰テストを持つ。Mac GUIでは日本語の確定、Ctrl+Shift+Jで改行が増えないこと、日本語コピー、全角・結合文字・絵文字、ANSI16色、ブロック・下線・バーのカーソルを確認した。設定を隔離したzshの終了コード7表示と再起動も確認済み。

IMEの候補選択・取消・再変換と、変換途中のセッション追加・終了・プロジェクト切り替えを組み合わせた実機確認は未完了。変換の確定先は回帰テストで検証している。実rust-analyzer結合テストは通常のテストから除外し、明示的に起動する。TypeScriptと各エージェントCLIの実機確認は別に必要。

終了コードはPTYの直接の子の値。ユーザーのシェル設定がKiroなどのラッパーを挟む場合、内側のシェルが7で終わってもラッパーは0で終わることがある。設定を変更せずに切り分ける場合は、アプリの起動環境で空のZDOTDIRを指定する。

検索・Git・外部変更検知には同期I/Oが残る（[#18](https://github.com/kmtym1998/eren/issues/18)）。未解決の項目は、linked worktree・サブフォルダのブランチ名表示（[#48](https://github.com/kmtym1998/eren/issues/48)）、末尾改行なしの分割差分（[#31](https://github.com/kmtym1998/eren/issues/31)）、引用付きリンクの空白（[#32](https://github.com/kmtym1998/eren/issues/32)）、worktreeパス内の改行（[#33](https://github.com/kmtym1998/eren/issues/33)）。最新の状態は [Issue一覧](https://github.com/kmtym1998/eren/issues?q=is%3Aissue+is%3Aopen) で管理する。新しい修正が入ったmainと、古い配布バイナリの挙動は区別する。

## 今後の対象

- アプリ終了後もプロセスを維持する常駐ホスト。
- LSPのdeclaration/typeDefinitionを開くUI。補完、rename、LSPによる整形。
- `.app`とCaskによる配布、公証。Homebrew tap公開はユーザー判断で保留している。

Windowsは現時点の対象外。LinuxとIntel Macはビルド設定があるが、すべてのGUI・IME操作を検証した対応環境とはしていない。

## 開発と配布

開発の前提と検証コマンドは [README.md](README.md) を参照。`rust-toolchain.toml` の固定Rustとコミット済みのCargo.lockを使う。PRとmainへのpushではMacのCIがfmt・テスト・Clippy・ネイティブビルドを実行する。Python要件、キャッシュ、Intel/Linuxと実LSP・GUIの確認範囲は [docs/ci.md](docs/ci.md) に記載する。通常の確認は次のとおり。

```bash
cargo fmt --all --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

[v0.1.0](https://github.com/kmtym1998/eren/releases/tag/v0.1.0) のmacOS arm64・Intel・Linux x86_64用成果物は既に存在する。現在の修正を配布したリリースではない。tapの公開とbrew導入確認は保留中で、過去の成果物があることとは分けて扱う。

`release.yml` はタグpushでリリース公開、手動実行でActionsの成果物を作る設定。新規リリースとtap公開は保留中。配布再開時は対象ごとのreleaseビルドと起動を確認する。既存成果物とtap再開の手順は [HOMEBREW.md](HOMEBREW.md) にある。
