# 開発時の検証手順

## 自動テスト

```sh
rustup component add clippy rustfmt
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

依存クレート取得済みの環境ではcargo test／clippyへ`--offline`を追加できる。
現在の53テストにはRPCのID衝突、文字列ID、null応答、承認境界、通知あふれ、
Turnごとのタイムアウト、切断・異常終了、モデル一覧のページ送り、画像／Schemaの転送を含む。
模擬CodexのHTTPテストは外部モデルを呼び出さない。

## 実Codexへの接続

[実接続テスト結果と再実行手順](live-codex-validation.md)を参照する。
`scripts/live_codex_smoke.py`は手動実行専用で、通常のcargo testには含めない。
実ログインとモデル利用枠を使用し、独立した一時環境で動作する。
強制終了テストが対象にするのは、その実行で起動した隔離App Serverだけである。

生成結果はモデルに依存するため、成功したHTTP往復と回答内容の正しさを分けて評価する。
既知のdetail=lowでの不一致は、テストを成功扱いに変更して隠さない。
`docs/live-codex-results.json`は試行履歴を含む証跡であり、全項目成功を示すファイルではない。

## リリース判断

主要経路のテスト成功だけで全API互換・無停止運転を保証しない。
変更した経路のテスト、対象Codex／モデルでの実接続、[対応表](app-server-coverage.md)と
既知制約の更新を行う。長時間・並列負荷、他Provider、新しいCodexバージョンの受入は別途必要。
