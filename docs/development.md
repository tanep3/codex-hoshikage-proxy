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
自動テストにはRPCのID衝突、文字列ID、null応答、承認境界、通知あふれ、
Turnごとのタイムアウト、切断・異常終了、モデル一覧のページ送り、画像／Schemaの転送、要求ID重複・永続化障害、承認ID・欠落通知、Steer・中断競合、会話モデル変更と復元を含む。
模擬CodexのHTTPテストは外部モデルを呼び出さない。Supervisor統合テストは子プロセス異常終了時のProxy非ゼロ終了と、SIGTERMでの正常終了を検証する。

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


## ストレージ障害の注入試験（Linux）

`cargo test --locked --test v2_io_faults` は、`cc` でテスト専用ライブラリを一時ディレクトリへ生成し、隔離した子プロセスだけに `LD_PRELOAD` で適用する。対象はその試験専用ディレクトリ内のwrite/pwrite/fsync/fdatasyncに限定し、保存物のwriteのENOSPC、同期のEIO、SQLite WALの書込・同期のEIOを再現する。共有ディスクを埋めたり、mountやホスト全体の障害設定を変更したりしない。`io_fault_fixture` は親試験から6条件で起動するため通常一覧ではignoredになる。この注入試験自体は実ファイルシステムの枯渇や突然の電源断を再現するものではない。容量不足は以下の専用試験で確認する。


## 実ファイルシステム容量不足・SQLite FULL（Linux）

```sh
python3 scripts/test_v2_full_fs.py
```

`unshare`、`mount`、Python 3、Rust開発環境が必要。ユーザー・マウント名前空間の作成権限を必要とするが、本番領域やホストのマウントを変更しない。スクリプトは通常テストから独立しており、作成できない環境では失敗して終了する。隔離できないままホスト領域へフォールバックしない。

子の名前空間だけに16 MiB・4096 inode上限のtmpfsを作る。実行時間60秒、CPU時間30秒、仮想メモリ512 MiBに制限し、試験側でも名前空間・ファイルシステム種別・容量上限を照合してから書き込む。試験終了・タイムアウト時は名前空間ごと破棄し、親からマウントが見えないことを確認する。tmpfsのメモリ使用量と少量の通常のテスト負荷は発生する。

実際にENOSPCになるまで埋め、成果物保存の失敗とSQLiteのSQLITE_FULLを確認する。空きを戻してストアを開き直し、`integrity_check`、インスタンス・世代、保存済み要求キー、未commit操作のrollback、UNKNOWNの再送禁止、失敗した原本の再コピー禁止を検証する。専用テスト `actual_full_filesystem_preserves_committed_state` は通常のcargo testではignoredであり、このスクリプトから明示実行する。

tmpfsは実ファイルシステムだがRAM上の領域であり、ext4のディスクイメージや実デバイスを満杯にする試験とは異なる。ブロックデバイスの故障・電源断・実ディスクの永続化特性は保証範囲に含めない。
