# eren — 仕様メモ（作業用）

Sirio / Herdr 風の、coding agent のためのネイティブ・ワークスペースを新規に作る。
Sirio の fork ではなく、GPUI + gpui-kit でゼロから組む（2026-11-15 決定）。
リポジトリは kmtym1998/eren（public — 2026-11-15 に Homebrew 配布のためユーザーが public 化）。

## 確定した方針

- UI は GPUI（Zed の GPU フレームワーク）+ gpui-kit（gpui-component 等の umbrella crate、依存は `gpui-kit` 1本に寄せる）
- VSCode / Monaco の埋め込みはしない。ビューアは GPUI ネイティブで作る
- コードジャンプは LSP を使う（簡易ジャンプ＝tree-sitter 名前引きは採用しない）
  - 対象：definition / declaration / typeDefinition / implementation / references / hover / documentSymbol
  - 飛ぶ系は全部「位置 → Location 群」で同型なので、`goto(kind, position)` に共通化する
  - 複数候補はピッカー、ナビゲーション履歴（戻る / 進む）を必須とする
  - 対応はサーバーの capability を見て出し分ける
- ファイル編集も対象にする（2026-11-15 ユーザー要望で、当初の読み取り専用前提から変更）。LSP 同期は全文同期のみ（外部変更時は開き直し）。補完・rename・整形は対象外
- 言語サーバーは最初 rust-analyzer のみ、次に TypeScript を足す

## UI の形（2026-11-15 ユーザー指示で変更）

- Herdr のように **ターミナルがメイン**。中央ペインは既定でターミナルが占める
- エディタは常時表示しない。ファイルを開いたとき／トグル操作のときだけ、ターミナルの横（または上）に表示する
- これに伴い、ターミナル（Phase 3 の予定だったもの）を Phase 1 の次に前倒しする

## MVP スコープ

### Phase 1（ビューア核）
- [ ] ウィンドウ + 3ペイン骨格（左：プロジェクト/agent、中央：ビューア、右：Changes）← 土台は作成済み
- [x] プロジェクトを開く（フォルダ選択 → ファイルツリー表示、`.git/HEAD` からブランチ名表示。CLI 引数でフォルダを渡しても開ける）
- [x] worktree 一覧と切替（2026-11-16、Codex が `src/worktrees.rs` を実装・助手が統合）：`git worktree list --porcelain` をパースし、2件以上あるときサイドバーに「ワークツリー」欄を出す。クリックでそのフォルダをプロジェクトとして開き直す。実リポジトリ＋linked worktree で表示と切替を GUI 確認済み
- [x] ファイルツリー → コードエディタ（EditorState 化済み 2026-11-15：そのまま編集でき、保存ボタン / Cmd+S・Ctrl+S で保存、未保存はドット表示。tree-sitter ハイライトは rust / typescript / tsx / javascript / toml / markdown / css / html / bash / yaml / python + json に対応。未保存のまま別ファイルへ切り替える際は確認バーで「保存して開く／破棄して開く／キャンセル」を選ぶ。ローカル実装済み 2026-11-15、未 push）
- [x] Changes：git status の変更一覧 + Diff 表示（unified、2026-11-15 ローカル実装）：右ペインに件数バッジ付きで変更ファイルを一覧（XY ステータスの色分けバッジ、更新ボタン、保存時にも自動更新）。クリックで `git diff HEAD`（未追跡は /dev/null との diff）を色付きでエディタ領域に表示。split 表示は今後の拡張
- [x] 横断検索（2026-11-15 実装）：ツリーパネル上部の入力欄に語を入れて Enter で、ignore クレートでプロジェクトを走査（.gitignore 尊重、2MB 超とバイナリは除外）し、部分一致（大文字小文字を区別しない、上限300件）の結果一覧を表示。クリックでファイルを開いて該当行へジャンプ（未保存時は確認バーを経由）

### Phase 2（LSP）
- [x] LSP クライアント基盤（2026-11-15 実装）：`src/lsp.rs` に自前の最小 JSON-RPC クライアント（stdio、読み取り専用スレッド＋同期リクエスト、全文同期）。当初案の async-lsp は使わず同期スレッド方式にした（GPUI のバックグラウンドタスクから呼ぶ）。initialize で capabilities を保持し、対応可否でジャンプを出し分ける
- [x] rust-analyzer で definition / implementation ジャンプ（2026-11-15 実装）：プロジェクト（Cargo.toml あり）を開くとバックグラウンドで自動起動。F12=定義、Ctrl+F12=実装、エディタヘッダーにもボタン。複数ヒットは候補リスト、戻る/進む履歴（Alt+←/→）付き。UTF-16 位置変換あり（単体テスト＋実サーバー結合テスト＋GUI 確認済み）。rust-analyzer は rustup の shim ではなく toolchain 実体を優先して解決する
- [x] references / hover / 診断表示（2026-11-15 実装）：Shift+F12・ヘッダー「参照」で参照検索（候補リスト表示）。hover はエディタ組み込みの HoverProvider に自前クライアントを接続し、実サーバーの内容を表示。診断は publishDiagnostics を取り込み、エディタ上部に重大度つきのバーを表示（クリックで該当行へジャンプ、40ms ポーリングで更新）
- [x] TypeScript サーバー対応（2026-11-16）：package.json のあるプロジェクトでは PATH 上の typescript-language-server を `--stdio` で起動（Cargo.toml があれば rust-analyzer を優先）。言語 ID は拡張子から判定（rs / ts / tsx / js / jsx）し、サーバーが扱える言語だけ didOpen する。GUI で定義ジャンプと hover を確認済み。前提：プロジェクト側に typescript が入っていること（サーバーが workspace 内を探す）。見つからない場合はステータスに理由を表示
- [x] 外部変更の再同期（2026-11-16）：開いているファイルの更新時刻＋内容ハッシュを約1秒ごとにポーリング。未保存の変更がなければ自動で読み直し（カーソル位置を保持、LSP へも再同期）、あれば上書きせずステータスに警告。削除も一度だけ通知する。notify クレートは使わずポーリングで実装
- [x] index 進捗表示（2026-11-16）：`$/progress`（workDoneProgress）をクライアントで保持し、ステータスバーに「LSP: タイトル 進捗%: メッセージ」形式で表示。rust-analyzer の Fetching / Roots Scanned が実際に出ることを GUI で確認済み。これで Phase 2 は完了

### Phase 3（ターミナル + agent 管理）
- [x] ターミナル pane の土台（前倒しで実施、2026-11-15 ローカル実装）：alacritty_terminal 0.25 + PTY でログインシェルをプロジェクトフォルダで起動。グリッド描画（色・太字・反転・ブロックカーソル）、キー入力（Ctrl 組み合わせ・矢印・Alt プレフィックス）、ペインサイズへの追従（canvas 実測で cols/rows を再計算）。中央ペインの主役として常時表示され、エディタはトグル表示。残り：マウス選択・コピー、日本語 IME 入力、複数ターミナル
- [x] スクロールバック（2026-11-15 ローカル実装）：履歴 10,000 行を保持し、ホイール／Shift+PageUp・PageDown で遡れる。遡っている間は右下にバッジを出し、キー入力で最下部へ戻る
- [x] ターミナルのマウス選択＋コピー／ペースト（2026-11-16）：ドラッグでセル範囲を選択（ハイライト表示）、Cmd+C / Ctrl+Shift+C でコピー、Cmd+V / Ctrl+Shift+V でペースト。入力・スクロール・セッション切替で選択解除。残りは日本語 IME のみ
- [x] 複数セッション＋agent 起動と状態表示（2026-11-16）：サイドバーにセッション一覧（クリックで切替、×で終了、状態ドット）。追加ボタンからシェル／Claude Code／Codex／OpenCode を起動でき、エージェント終了後はシェルに落ちる。状態は可視テキストからのヒューリスティクス判定（`src/agents.rs`：作業中=青／入力待ち=琥珀／不明=灰）で、Codex の入力待ち検出を実機相当の TUI で確認済み
- [x] レイアウト永続化（2026-11-16）：プロジェクト・セッション構成・アクティブセッション・開いているファイル・エディタ表示状態を `~/.local/state/eren/state.json` に保存し、引数なし起動時に復元（プロセスは作り直し）。終了→再起動で Codex セッション＋ファイルが復元されることを GUI で確認済み
- [ ] プロセス永続化（終了してもプロセスが生きる仕組みは Herdr / Sirio の background host 方式を参考。将来課題）

## 既知の制約・注意

- GPUI は pre-1.0。gpui-kit が pin する gpui-pre スナップショットに追随する（kit 経由で使うので直接 GPUI を依存に書かない）
- gpui-component の Editor は LSP を Provider trait（Completion / Hover / Definition / CodeAction）で受ける設計。implementation は trait に無い可能性があり、その場合は独自アクションから LSP クライアントを直接叩く
- 開発 VM（Muse 側）は GUI が無いので実機確認はユーザーのマシンで `cargo run` する。VM 側では `cargo check` まで
- Sirio は MIT だが無名の個人プロジェクト（stars 0 / 2026-07 開始）。fork はしないが、ターミナル等の実装は参考にする

## 動かし方

```bash
cargo run
```

要 Rust 安定版ツールチェイン。

## 配布（Homebrew）

- リリースは GitHub Actions（`.github/workflows/release.yml`）で、`v*` タグを push すると macOS arm64 / macOS Intel（cross）/ Linux x86_64 の release バイナリを tar.gz + sha256 にして GitHub Release へ添付する
- Homebrew は個人 tap `kmtym1998/homebrew-tap` の Formula で配る想定。雛形は `dist/homebrew/eren.rb`（tarball を落として `bin/eren` を入れるだけ、test は `eren --version`）
- **前提：release の tarball は認証なしで落とせる必要があるので、配布を始める時点で eren repo を public にする**（private のままだと他人の brew が 404 になる）
- `brew install kmtym1998/tap/eren` で入ってターミナルから `eren` で起動、が v0 の形。.app 化 + Cask 化と公証（notarization）は後続
- バイナリは `--version` / `--help` を画面なしで処理する（formula の test とスクリプト用）
