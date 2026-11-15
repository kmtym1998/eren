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

## MVP スコープ

### Phase 1（ビューア核）
- [ ] ウィンドウ + 3ペイン骨格（左：プロジェクト/agent、中央：ビューア、右：Changes）← 土台は作成済み
- [x] プロジェクトを開く（フォルダ選択 → ファイルツリー表示、`.git/HEAD` からブランチ名表示。CLI 引数でフォルダを渡しても開ける）※ worktree 一覧は未
- [ ] ファイルツリー → コードビューア／エディタ（ツリーとプレーンテキスト表示＋行番号は実装済み 2026-11-15。残りは EditorState 化＝そのまま編集して保存できるようにすること、と tree-sitter ハイライト）
- [ ] Changes：git status の差分ツリー + Diff 表示（split / unified）
- [ ] 横断検索：ripgrep 系 crate（grep-searcher + ignore）で結果一覧 → 行ジャンプ

### Phase 2（LSP）
- [ ] LSP クライアント配線（lsp-types + async-lsp 系、サーバー管理は (server, workspace root) キー）
- [ ] rust-analyzer で definition / implementation / references / hover
- [ ] UTF-16 位置変換、index 完了待ち、外部変更の再同期（watcher）

### Phase 3（ターミナル + agent 管理）
- [ ] ターミナル pane（GPUI に出来合いが無い難所。Paneflow の alacritty_terminal 実装、Sirio の libghostty-vt 実装を参考に移植を検討）
- [ ] agent 起動（Claude Code / Codex 等をターミナル pane で）と状態表示（running / waiting / done）
- [ ] セッション永続化（終了してもプロセスが生きる仕組みは Herdr / Sirio の background host 方式を参考）

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
