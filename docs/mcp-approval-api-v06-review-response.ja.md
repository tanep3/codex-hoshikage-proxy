# API 0.6 — GatewayレビューC06-01/02への回答

2026-09-17。両指摘を受け入れて具体契約へ補完した。**Proxy側の追記・文書検査とGatewayによる再照合は完了。利用者から接続Goと実装指示を受領した。**

正本：[API 0.6 第12〜13節](mcp-approval-api-v06.ja.md)、[JSON例](mcp-approval/api-v06-examples.json)、[境界検査記録](mcp-approval/api-v06-boundary-validation.json)。

| 指摘 | 対応 |
| --- | --- |
| C06-01 応答上限 | 表示／操作詳細64KiB、単体Response／interaction256KiB、grant一覧1MiB、interaction一覧64MiB、capability全体1MiB（追加部分64KiB）を表で固定。件数はgrant16、interaction256、未回答16。旧profileのoperation256KiBを維持 |
| C06-01 メタデータ総量 | policy descriptor2KiB、実効policy4KiB、grant48KiB、interaction／Response192KiBの将来状態予約を検証。配列各項目だけでなく全体も登録前に検証。登録・発行超過と異常GETのHTTP/codeを定義 |
| C06-01 取消可能性 | 最大件数を発行して終端化しても一覧上限内。既存IDの取消・stopは一覧GET成功を前提にしない。古い記録を削って埋め合わせない |
| C06-02 期限 | 202の永続受付commitから60秒。UTC期限を返し、ホスト単調時計も使用。GET・再起動で延長しない。残時間が証明不能なら期限切れ |
| C06-02 状態 | preparationに期限・復旧・Turn送信境界・設定隔離を公開。policy／Response／response.create／占有の対応表を追加。設定だけ不明ならAI未開始を示しつつ占有保持 |
| C06-02 停止 | 既存POST /v2/codex/stopsで元要求キーまたはResponse IDを指定。停止、ready、Turn送信意思を直列化。遅着準備から開始しない |
| C06-02 復旧 | 同じResponse/binding/selection/deadlineで照合。送信済み設定操作を再送しない。policy_setup_unknown確定後は元要求を実行せず、隔離を確認して失敗を確定 |

新しい応答フィールドはcapability.response_limits／policy_preparation_timeout_msとexecution_policy.preparation。policy.reasonへpolicy_setup_timeout／policy_setup_cancelledを追加。JSON例にも全て反映した。

C06-S01〜04、C06-P01〜05を受入へ追加。例のサイズ・照合値・予約量の検査を行ったが、製品のRPC・競合・再起動試験ではない。全ツール解析を着手条件に戻していない。コード・設定・サービス変更なし。
