# Gatewayとの対話要求中継：具体化案

2026-09-13。**旧設計案（履歴）**。Proxy側は[実装済みの追加API契約](interaction-api.ja.md)へ具体化した。GatewayのUI実装・結合受入は未完了。本資料より追加API契約を優先する。

## 責務

ProxyはCodexの要求を受信し、Response・会話・ワーク・Thread・Turnとの対応を確定する。要求の保存・期限・回答検証・上流への送信意思と結果・監査を所有する。GatewayはDiscordへの表示、操作者の本人確認・権限判定、回答操作の受付と同じ要求キーによる照会復旧を所有する。GatewayがCodex App Serverへ直接応答しない。

クライアント定義Function Callingとは別機能。OpenAI互換APIに独自の質問イベント・回答形式を必須にせず、Gateway向けv2拡張で提供する。

## 提案するAPI

- 実行受付に対応する対話種別の明示指定を追加する。指定なしは現行クライアントとして扱い、対話不能時の明示エラーと停止を維持する。対応していないGatewayへ質問を出して待たせない。
- `GET /v2/codex/responses/{response_id}/interactions`：要求一覧と状態をスナップショットで取得。SSE通知は再照会のきっかけとし、配信保証に使わない。
- `GET /v2/codex/interactions/{interaction_id}`：対象・要求内容・状態・revision・回答期限を取得。
- `POST /v2/codex/interactions/{interaction_id}/reply`：`Idempotency-Key`、復元世代、対象revisionを照合して回答を受理する。

要求の表示例：

```json
{
  "interaction_id": "int_example",
  "response_id": "resp_example",
  "conversation_id": "conv_example",
  "workspace_id": "ws_example",
  "kind": "user_input",
  "state": "pending",
  "revision": 1,
  "expires_at": "2026-09-13T01:10:00Z",
  "request": {
    "questions": [{"id":"color","header":"色","question":"どの色にしますか？","isOther":true,"isSecret":false,"options":[{"label":"青","description":"青を使用"}]}]
  }
}
```

回答例：

```json
{"expected_revision":1,"response":{"answers":{"color":{"answers":["青"]}}}}
```

## 応答種別と検証

- 質問：上流の質問IDに対応する回答だけを受理する。存在しないID・サイズ超過・必須回答不足を拒否する。選択肢を勝手に許可に読み替えない。
- MCP：form・URL確認を区別する。acceptには要求schemaに従うcontent、decline/cancelにはnullを使う。URLは勝手に取得・実行しない。表示できないschemaは受付時点で非対応とする。
- 権限：要求された権限の部分集合だけを許可し、既定は当該Turnに限定する。通常のコマンド承認の `decision` を流用しない。セッション永続許可は別の明示選択とする。

JSON Schemaの検証範囲、サイズ・件数・期限の具体値、Gatewayが実際に表示できる種別は実装前に確定する。未対応schemaを「検証済み」としてそのまま上流へ転送しない。

## 状態・競合・復旧

- `pending` → 回答の送信意思を永続化 → `replying` → 書込確認。上流の `serverRequest/resolved` まで受け取っても、要求が解消したことと実行成功は区別する。
- 回答送信後に通信・保存結果が不明なら `unknown`。同じPOSTを再受信しても上流へ再送せず、保存済み状態を返す。異なる内容で同じキーを使った場合は409。
- 期限切れ・停止・Turn終了・上流による解消後の回答は拒否する。停止意思が先に確定した場合、後からの許可を上流へ送らない。
- Proxy再起動・App Server切断・正式復元で古いRPC IDを別接続へ流用しない。復元世代を照合し、元のAI要求を再実行しない。
- 質問の回答やMCPフォームは秘密を含み得る。公開イベント・通常ログ・無期限の監査本文には保存しない。監査には対象、操作時刻、判定、送信状態を残し、回答本文の保持期限は別途定める。

受入では回答／停止／期限切れ／上流解消の競合、同一キー再送、書込前後の強制終了、世代不一致、別利用者による回答拒否を双方で確認する。既存の正常系画像配信を回帰試験に含める。
