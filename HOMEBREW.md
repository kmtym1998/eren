# Homebrew 配布メモ

状態：**一旦保留**（2026-11-15 ユーザー判断「めんどいから一旦なし」）。再開するときはこのファイルの手順どおりに進めればよい。

## 完了済み

- リリース自動化 `.github/workflows/release.yml`：`v*` タグで macOS arm64 / macOS Intel（cross）/ Linux x86_64 をビルドし、tar.gz + sha256 を GitHub Release に添付する。ランナーは `macos-15`（`macos-14` は 2026-11-02 に廃止済みなので使わないこと）
- v0.1.0 リリース成功（run #3、対象 commit `043f972`）：https://github.com/kmtym1998/eren/releases/tag/v0.1.0
  - `eren-0.1.0-aarch64-apple-darwin.tar.gz` sha256: `8bf113e54eb7b176fe3fa8f312f1372eb0b3ec3547485e592a26374c76d63642`
  - `eren-0.1.0-x86_64-apple-darwin.tar.gz` sha256: `45b2eb31f38da77fb015dcfb075b4c1412c837b9bd98d695498146f3a796c276`
  - `eren-0.1.0-x86_64-unknown-linux-gnu.tar.gz` sha256: `ed65b326d3b19b00414a07017e89e5d760b495254f34a44757a72d2afa905fb4`
- Formula は上記 sha256 を入れて完成済み。Muse のワークスペース `~/workspace/homebrew-tap/` に `Formula/eren.rb` と README.md を用意してある（このリポジトリの外なので、消えた場合は `dist/homebrew/eren.rb` の雛形に sha256 を入れ直せば再構成できる）

## 再開時の残手順

1. ユーザーが空の **public** リポジトリ `kmtym1998/homebrew-tap` を GitHub で作成する（README 等は付けない）
   - Muse の GitHub コネクタではリポジトリ作成ができない（`create_repository` が 403 `Resource not accessible by integration`。2026-11-15 確認）。ファイルの push はコネクタで可能
2. `Formula/eren.rb`（完成版）と `README.md` を homebrew-tap へ push する（コネクタの書き込み承認カードが出る）
3. Mac で `brew install kmtym1998/tap/eren` を試し、`eren` が起動するか確認する
   - バイナリは未公証なので、初回起動時に macOS の許可（システム設定 > プライバシーとセキュリティ）が求められる場合がある

## 次回リリース時の更新

1. `v*` タグを打って Release を作り直す（タグ作成もコネクタではできないので、ユーザーが git か GitHub 画面で実施）
2. Release の `.sha256` 資産の値で formula の `version` と sha256 を差し替える
3. 差し替えた formula を homebrew-tap の `Formula/eren.rb` にコピーして push する
