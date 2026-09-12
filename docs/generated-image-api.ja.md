# v2生成画像の自動成果物登録

2026-09-12。Proxy実装契約。GatewayのDiscord自動配信は別途対応する。[v2基本契約](workspace-artifact-api-v2.ja.md)に対する後方互換の追加で、`contract_version` は `2.0` を維持する。

## 概要と責務

Proxyは受け付けたResponseの実行終了後、記録済みThread ID・Turn IDを使ってCodex App Serverの履歴を照会する。対象Turnの `itemsView: full` と終端状態を確認し、`imageGeneration` の `result` に含まれるPNGを、不変のv2成果物として自動保存する。回答本文、画像保存ディレクトリの更新時刻、別Turnの画像から帰属を推測しない。PNG保存先が存在しなくても、上流の画像本体から保存できる。

Gatewayは登録状況を照会し、`ready` の成果物を既存の取得・リース経路でダウンロードしてDiscordへ添付する。ProxyはDiscordへ投稿しない。OpenWebUI用v1画像取得APIとOpenAI互換APIは継続する。

上流の基本仕様は[公式App Serverドキュメント](https://developers.openai.com/codex/app-server)を参照。画像フィールドは、対象CLI 0.153.4の生成JSON Schemaと実際の `thread/read` 応答で確認した。`imageGeneration.result`、`itemsView: full` を確認できない上流を、画像なしとして扱わない。

## 提供判定

`GET /v2/codex/capabilities` の `features`：

```json
{"generated_image_artifacts":true,"response_generated_images":true}
```

この追加機能で追跡を開始するのは、対応版で新規受付したResponse。対応前のResponseは `409 generated_images_not_tracked`。過去の画像を一括してDiscordに再配信する移行処理は行わない。Gatewayはこのエラーで回答を再実行しない。

## 登録状況の取得

`GET /v2/codex/responses/{response_id}/generated-images`

Bearer認証、`X-Proxy-Instance-Id`、`X-Proxy-Recovery-Generation`、ワークのアクセス制御を既存v2と同じ条件で適用する。

```json
{
  "response_id":"resp_r1",
  "conversation_id":"conv_c1",
  "workspace_id":"ws_w1",
  "revision":5,
  "state":"complete",
  "items":[
    {"image_id":"img_example","ordinal":0,"state":"ready","artifact_id":"art_a1","error":null},
    {"image_id":"img_other","ordinal":1,"state":"failed","artifact_id":null,"error":{"code":"invalid_generated_image"}}
  ],
  "error":null,
  "expires_at":"2026-09-19T09:00:00Z"
}
```

全件を一貫したスナップショットで返す。ページングはしない。画像本体・上流のプロンプト・ローカルパスは含めない。成果物のMIME、サイズ、SHA-256、現在の失効・破損状態は `GET /v2/codex/artifacts/{artifact_id}` で取得する。

| 状態 | 意味 |
| --- | --- |
| 全体 `pending` | 実行または登録状況の確定待ち。空配列でも画像なしは未確定 |
| 全体 `complete` | 対象Turnの全画像について登録判断が確定。全件成功の意味ではない |
| 全体 `unknown` | 帰属、一覧の完全性、上流照会などを確定できない |
| 項目 `creating` | 登録判断・保存中 |
| 項目 `ready` | 登録成功。`artifact_id` が存在する。後日の失効等は成果物側で判定 |
| 項目 `failed` | 登録失敗。エラーを参照する |
| 項目 `unknown` | 保存成否等を確定できない。同じ画像の再作成・AI再実行の根拠にしない |

`complete` と空配列の組み合わせだけが画像なし確定。`complete` に `creating` は残さない。`complete` 後は画像項目を追加しないが、再起動時に保存済みmanifestから登録成功を復元できれば、項目の `unknown / failed` を `ready` に修復しrevisionを更新する。

`image_id` はResponse IDと上流画像項目IDから決める。`ordinal` は0始まりの対象Turn内の画像順。同じIDの再検出で新しい成果物を作らない。別Responseの同一内容画像は別の成果物になる。専用ツールで作業ファイルとして別途公開したコピーは、元の画像項目IDとの対応を証明できないため自動統合しない。ハッシュ一致だけの重複除去は行わない。

## 監視と保持

- 通常は5秒周期の保守処理で実行終端後の照合を開始する。画像照合と保存は既存コピー並列枠を使い、文章の保存と実行停止を待たせない。
- `thread/read` は1回30秒でタイムアウト。照会不調などは60秒間隔で再照合する。最初の照合から既定600秒で確定しなければ `unknown` とする。不完全な一覧・ID衝突など、確定不能が明らかな場合は直ちに `unknown`。
- `unknown` も照会保持期間内に限り再照合する。原依頼の再送や、予約済み成果物の原本再コピーは行わない。Gateway側の待機期限を超えた場合も、Proxyの照会保持期間内なら状態を再取得できる。
- `expires_at` は最初の照合開始から既定7日。実行中・照合開始前は `null`。期限後は `410 generated_images_expired`。一覧自体にリースはないため、Gatewayは発見した画像ID・成果物ID・配信記録を永続化する。
- 画像本体は通常の成果物保持期間と有限リースに従う。一覧の期限と本体の期限は独立する。
- イベント `response.generated_images_changed` は `{ "response_id":"resp_r1", "revision":5 }` を通知する。再配信保証はなく、照会の補助として扱う。既存 `artifact.ready` も使用できる。
- `response.output_ready` は従来どおり回答本文の保存完了。画像登録完了とは独立する。画像だけの回答、回答保存失敗、失敗・中断したTurnの登録済み途中成果も照会できる。

## 制限とエラー

`[v2]` とcapabilitiesの `limits` に以下を追加する。受理時点の件数・画像サイズ・照合期限・一覧保持期間をResponseに記録する。

| 設定 | 既定値 | 許容範囲 |
| --- | --- | --- |
| `generated_images_max_count` | 16 | 1〜64 |
| `generated_image_max_bytes` | 10485760（10 MiB） | 1〜16777216 |
| `generated_images_settle_seconds` | 600 | 1〜315360000 |

画像サイズには現在の `artifact_max_bytes` も適用する。保存容量、空き容量、コピー並列数、成果物保持・リース上限は既存設定を使う。画像はPNG署名・IHDR・非ゼロ寸法とサイズ上限を検証し、`image/png` で公開する。完全な画像デコードは行わない。

件数超過は全体 `unknown / generated_images_limit_exceeded` とし、上限までの画像を黙って返さない。画像1件の上限超過はその項目の `failed / generated_image_too_large`。一部失敗でも他の正常画像を保存する。

主な状態内エラーは `image_inventory_unavailable`、`image_inventory_incomplete`、`image_turn_unresolved`、`image_turn_mismatch`、`image_identity_invalid`、`image_inventory_changed`、`image_generation_failed`、`image_generation_unknown`、`invalid_generated_image`、`image_capture_unknown`。通常の容量・アクセス・保存エラーも状態に記録する。HTTP照会自体のエラーは基本契約の認証・世代・対象存在・アクセス制御に加え、上記409/410を使用する。

## 復旧と試験

画像識別子を永続化してから成果物を予約し、画像本体、SHA-256等のmanifest、DBの順に公開する。画像本体の保存と画像一覧の成功記録の間で停止しても、既存操作キーから同じ成果物へ再接続する。未確定の予約は別内容で作り直さず `unknown` にする。正式復元の保留中は新たな画像登録を行わない。

自動試験は `tests/v2_images.rs` と `tests/v2_http.rs`。実画像の保存経路だけを再検証するignored試験、および新規生成・自動登録・HTTP取得・再起動を隔離環境で確認する `scripts/live_v2_images.py` を提供する。実Discord表示はGatewayの変更後に別途確認する。
