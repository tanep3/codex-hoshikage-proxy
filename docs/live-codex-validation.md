# 実Codex接続テスト

実施日: 2026-09-11（JST）

- Codex CLI: `0.153.4`
- モデル: `chatgpt/gpt-5.6-luna`
- ソース: バグ修正コミット`72a636e`＋作業ツリーの機能追加
- 一時ワークスペース、独立したProxy・App Server、read-only sandboxを使用。
- 既存ログインの認証ファイルを権限600で一時領域へコピーし、終了時に削除。既存設定・会話・常駐サービスは変更しない。

## 結果

| 確認内容 | 結果 |
| --- | --- |
| initialize / initialized、モデル一覧 | 成功。6モデル取得 |
| Responses通常応答、previous_response_idでの会話継続 | 成功 |
| Responses画像入力＋JSON Schema、detail=high | 成功。赤画像をredと回答、Schemaに一致 |
| Chat画像入力＋JSON Schema＋reasoning_effort=low、detail=high | 成功 |
| Responsesストリーミング | 成功。複数deltaの結合結果と完了イベントを確認 |
| Chatストリーミング | 成功。複数deltaの結合結果と[DONE]を確認 |
| 応答待ち中のHTTPクライアント切断 | 成功。実CodexのTurnがinterruptedへ遷移 |
| 対話承認のcancel、二重回答拒否 | 成功。ファイルを書き込まず、二重回答に409 |
| 承認期限切れ | 成功。expiredに遷移し、ファイルを書き込まず |
| 隔離したApp Serverの強制終了 | 成功。HTTPストリームにruntime_disconnectedを即時通知 |
| Chat画像入力、detail=low | **不一致**。赤一色のPNGをblueと回答。2回再現 |
| Proxyを経由しないApp Server直接接続、detail=low | **同じ不一致を再現** |
| Proxyを経由しないApp Server直接接続、detail=high | 成功。redと回答 |

## 切り分け

`detail=low`の誤認はProxyを経由しなくても再現するため、この環境ではProxy固有の変換問題ではない。
Codex内部の画像処理とモデルのどちらに原因があるかまでは特定していない。全モデルに一般化もしない。
暫定的にはこのモデルで`detail=high`を使用する。Proxyがlowをhighへ黙って置換する変更は行っていない。

初回のストリーム検証は、文字列`STREAM_OK`が単一deltaに含まれると仮定して誤判定した。
実際には`STREAM`と`_OK`に分割されて正常配信されており、テスト側で結合して再検証済み。

実Codexが提示した承認選択肢は`accept`と`cancel`だった。提示されていない`decline`は409となることを確認し、
提示された`cancel`を用いた拒否を再検証した。標準の`decline`が提示されるケースの実機往復は今回未検証。

ID衝突、null応答、文字列ID、大量通知、複数パスの境界などは実サーバーで任意に発生させていない。
これらは既存の模擬サーバー・単体テストによる検証を維持する。
モデル一覧の実接続は確認したが、6件の一覧は1ページに収まるためページ送り・循環カーソルは模擬テストで検証する。

## 再実行

```sh
cargo build --offline --bin codex-hoshikage-proxy
python3 scripts/live_codex_smoke.py
```

既存のCodexログインを使う手動テストであり、モデル利用枠を消費する。通常のcargo testには含めない。
選択実行は`LIVE_CODEX_TESTS`へテスト名をカンマ区切りで指定する。モデルは`LIVE_CODEX_MODEL`で指定する。

```sh
LIVE_CODEX_TESTS=direct_image_low,direct_image_high python3 scripts/live_codex_smoke.py
```

未解消の画像誤認を隠さないため、lowのケースが不一致ならスクリプトは終了コード1を返す。
機械可読の結果と過去の試行は[live-codex-results.json](live-codex-results.json)へ保存する。

## 2026-09-11 制御API v1とモデル変更の追加検証

隔離した一時Proxy、Codex CLI 0.153.4で実施。常駐サービスには適用・再起動していない。

| 項目 | 結果 |
| --- | --- |
| Responses/Chatストリーム回帰 | 成功 |
| 開始ヘッダーのResponse/Thread/Turn ID | 成功 |
| 要求ID照会と同一要求の重複実行防止 | 成功 |
| 監視SSEのsnapshot・接続解除 | 成功 |
| expectedTurnId付きSteer、明示中断、終端後の再中断 | 成功 |
| 同じ会話でgpt-5.6-luna → gpt-5.6-terra | 成功。Thread IDを維持し、事前に記憶させたCEDARを回答 |
| 一時Proxy再起動後に旧Responseからモデル省略で継続 | 成功。terraを維持し、CEDARを回答 |
| UUID承認ID、cancel、重複判断拒否、承認期限切れ | 成功。拒否・期限切れでは指定ファイルを作成しない |
| HTTPヘッダー受信直後のTCP切断 | 修正後成功。0.31秒のテストでinterruptedを確認 |

切断試験では実際の不具合を確認した。App Serverはturn/start応答直後に一時的に
`no active turn to interrupt (-32600)` を返す場合があり、従来の内部中断は結果を無視していた。
この明示的な拒否に限った短時間再試行を加え、同一Threadの次の開始との競合も制御した。
受付成功・通信結果不明を理由にRPCを再送する処理ではなく、生成要求も再送しない。
模擬上流でも初回中断拒否と実TCP切断を再現する回帰テストを追加した。

承認の初回再テスト失敗は、試験側が旧連番IDを推測していたことによる。
有効承認一覧APIでUUIDを取得する方式に改め、拒否と期限切れを再検証した。
失敗試行はJSON履歴に残している。

開始応答喪失・ストア破損/書込失敗・大量通知欠落・同時同一要求・制御の終端競合・モデル変更拒否・Provider間切替拒否は模擬試験。
Hoshikage/Ollamaの実機切替、全モデル間の互換性、長時間負荷は今回の実機検証に含めない。

```sh
LIVE_CODEX_TESTS=control_api,conversation_model_change,responses_stream,chat_stream python3 scripts/live_codex_smoke.py
LIVE_CODEX_TESTS=approval_decline,approval_timeout,disconnect_interrupt python3 scripts/live_codex_smoke.py
```

モデル切替試験の対象は`LIVE_CODEX_SWITCH_MODEL`で指定可能（既定候補gpt-5.6-terra）。
この試験が再起動するのはスクリプト自身の一時Proxyのみ。
