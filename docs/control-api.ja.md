# 汎用実行制御 API v1（Gateway への契約回答）

2026-09-11。`proxy-change-request.ja.md` P-01〜P-09 に対応する契約。
Discord の認可・待ち行列・並列数 2・添付取得・表示復旧は Gateway が担当する。

## 認証・対象範囲

全 API は既存 Bearer 認証を使用する。現在は単一の共有管理主体であり、同じ API Key の利用者間に所有者分離はない。クライアント名の自己申告を認証と見なさない。操作可能な対象はこの Proxy が記録した Turn と承認に限る。API Key を Discord 利用者に渡さず、Gateway が操作権限を判定する。複数の信頼境界が必要なら別 Proxy を使用する（マルチテナント ACL は未実装）。LAN bind、既存 sandbox、ネットワーク、cwd 設定は変更しない。

## 開始・要求 ID

`POST /v1/responses` の任意 HTTP ヘッダー `Idempotency-Key` にクライアント生成の要求 ID（ASCII 英数字、`-_.`、1〜128 文字）を指定する。本文は既存形式。

```http
POST /v1/responses
Authorization: Bearer <key>
Idempotency-Key: req-<uuid>
Content-Type: application/json

{"model":"chatgpt/gpt-5.6-luna","input":"調査してください","stream":true,"metadata":{"codex.approval_capability":"interactive","codex.auto_approve_workspace":"false"}}
```

開始応答のヘッダー `x-response-id`、`x-codex-thread-id`、`x-codex-turn-id` が識別子。ストリーム本文の既存形式は維持する。Gateway は Responses 接続を完了まで維持する。切断時の中断は従来どおりで、監視 SSE は実行の所有者ではない。

`GET /v1/codex/requests/{client_request_id}` で記録を取得する。同一 ID・同一本文の再 POST は新規実行せず `200` の JSON 記録（ストリームの再送ではない）、異なる本文は `409 request_conflict`。重複判定は JSON キー順を正規化した本文全体。要求 ID は共有認証主体内で一意にする。

`received` → `dispatching` → `started` が送信境界。上流送信前に記録を同期保存し、開始確認後に Thread/Turn を保存する。切断・クラッシュで開始結果が不明なら `unknown`。再起動時に未確定な送信を再実行しない。記録なしは `404 request_not_found` であり「未送信の証明」ではない。記録を自動削除しない。ストア破損は起動失敗、書込障害は `503 control_store_unavailable` として以後 fail closed。バックアップ巻戻し・手動削除をまたぐ重複防止は保証しない。

状態ディレクトリは単一Proxyプロセスで専有する。複数Proxyで同じストアを共有する運用は非対応。

本文・最終回答は新規ストアへ保存せず、要求ハッシュと実行メタデータのみ保存する。保持期限は無期限（自動削除なし）。今後の削除・容量管理は要求 ID の再利用防止と合わせて協議する。

## 状態・継続・出力

`GET /v1/codex/turns/{turn_id}/status` は上流の現在照会結果 `status` と `last_observed_status`、`last_observed_at_ms`、`queried_at_ms` を分ける。上流で確認不能なら現在状態は `unknown`。保存済み終端状態を成功推測に置き換えない。

`GET /v1/codex/responses/{response_id}` で実行記録を取得できる。`previous_response_id` は従来の成功完了記録のみ利用可能。新規記録の実行中・初回中断・失敗・不明は `409 response_not_continuable`。初回失敗時は新規会話、後続失敗時は最後の成功 Response から継続可能だが、Thread の途中変更や上流履歴を巻き戻す意味ではない。同一 Thread に未確定実行がある場合は新規開始を拒否する。

最終回答の再取得は v1 では **非対応**。上流履歴からの再構成 API も公開しない。Gateway が受信済み出力を保存し、取り逃しは「取得不能」と表示する。出力保存の期間・容量が未確定のため新規保存は追加しない。出力欠落、失敗、中断を理由に自動再実行しない。

## 中断・Steer

- `POST /v1/codex/turns/{turn_id}/interrupt`（本文なし）: `202` は上流への受付確認のみ。終端済みは `200 already_terminal`、重複は再送せず前回の受付状態を返す。不明時は `409 turn_state_unknown`、不存在は `404 turn_not_found`。実際の中断は状態照会で確認する。
- `POST /v1/codex/turns/{turn_id}/steer`: `{"expected_turn_id":"...","input":"追加指示"}`。期待 ID 必須。モデル・cwd 等の追加フィールドは禁止。`202 accepted` は同じ Turn への入力受付であり指示遵守を保証しない。対象不一致・終端・中断要求中・承認待ちは `409`。上流での競合拒否も成功扱いしない。制御要求 ID・自動重複排除は Steer に提供しない。応答喪失は結果不明であり自動再送禁止。

両制御 API は Provider の実行枠を取得しない。開始直後に上流が明示的に `no active turn to interrupt (-32600)` と拒否した場合だけ、対象Threadの次の開始と競合しない状態で最大20回・50ms間隔の短い再試行を行う。通信結果不明や受付成功を理由に中断RPCを再送しない。中断はファイル変更の巻戻しや、すべてのバックグラウンド子孫プロセスの停止を保証しない。

## 承認・制約

承認 ID は UUID を含み再起動で再利用しない。旧 ID は `404`。判断は既存 `POST /v1/codex/approvals/{id}` の `decision`、任意 `expected_turn_id` / `expected_thread_id` による対象照合。`expires_at_ms` は期限、`reply_status` は `not_sent` / `unknown` / `written`（上流への書込成功であり、処理完了の保証ではない）。期限切れ・終端・重複判断は `409`。現在の未処理承認は `GET /v1/codex/turns/{turn_id}/approvals`。上流回答経路が失われた承認は復元しない。

`metadata["codex.auto_approve_workspace"]="false"` は自動承認を抑制する。`true` はグローバル上限を変更しない。抑制済み Thread は継続時も抑制を維持する。全操作で承認を発生させる指定ではない。同一 Thread の新規 Turn を競合拒否し、承認設定の上書きを防ぐ。Steer・承認・中断は新規 Turn のロックを取得しない。

## イベント再接続・Capability

既存 `GET /v1/codex/turns/{turn_id}/events/stream` は接続時 `codex.turn.snapshot`（状態・有効承認）を送る。接続単位の UUID + 連番を SSE `id` とする。再接続は履歴再配信ではなく snapshot から再同期する。`codex.events.gap` は受信欠落、`codex.events.reset` は新しい監視接続を示す。`Last-Event-ID` で過去の delta を復元する保証はない。承認は `approval_id` で重複排除する。snapshot と通知は原子的ではないので判断時に再検証する。

`GET /v1/codex/capabilities` は契約バージョン、実装済み機能、制限を返す。`readyz` と独立。モデルの Steer 受理は上流の状態にも依存する。

## モデル変更・添付・残る運用条件

同一 Provider 内のモデル変更は次の Turn の `model` に指定する。同じ Thread を `thread/resume` し、履歴を削除・要約置換せず `turn/start.model` へ転送する。各 Response のモデル記録はその実行時の値を維持する。モデル省略時は、参照した Response の古いモデルではなく、会話で最後に開始確認できたモデルを引き継ぐ。明示的な開始拒否では選択を進めない。開始応答を受け取れない場合は結果不明として会話の新規実行を拒否する。実行途中で失敗した Turn でも、開始確認済みのモデル選択自体は保持する。

Provider を跨ぐ切替は `409 cross_provider_model_change_unsupported`。モデル未登録は既存モデル解決エラー、上流の明示的な開始拒否は `502 runtime_error` と記録 `phase=rejected`。再試行する場合は新しい要求 ID を用いる。同一要求 ID は拒否済み記録を返し再実行しない。実行中の切替は `409 thread_busy`。旧 Response の参照は履歴分岐・巻戻しではなく同じ Thread の継続である。

添付は画像・UTF-8 テキスト入力・成果物返送。画像は既存入力を使用し、UTF-8 テキストの検証・読み込みとプロンプトへの組み込み、Discord への成果物送信は Gateway が担当する。汎用ファイル転送 API は追加しない。成果物に Gateway がアクセスする経路・必要な cwd ルートは運用側で別途確定する。

開始送信境界、ID 重複、ストア再読込・破損、旧データ、古い承認 ID、同一 Thread 競合、Steer/中断競合、再接続 snapshot、出力非対応、モデル変更後の文脈・失敗・再起動を模擬上流で検証する。実 Codex は独立した一時 Proxy/ワークスペースで検証し、常駐サービスの停止・再起動試験は行わない。

参考: [公式 Codex App Server](https://learn.chatgpt.com/docs/app-server) の `turn/steer`、`expectedTurnId`、`turn/interrupt`。

## 応答例と状態の読み方

要求照会の例（ハッシュ・本文・生成出力は公開しない）:

```json
{"response_id":"resp_<uuid>","client_request_id":"req-<uuid>","sequence":1,"phase":"started","thread_id":"thread-id","turn_id":"turn-id","model_id":"chatgpt/gpt-5.6-luna","last_observed_status":"inProgress","last_observed_at_ms":1789080000000,"started_at_ms":1789080000000,"suppress_auto_approval":true,"interrupt_state":null,"output_retrieval":"unavailable"}
```

`phase` は受付・送信の記録であり、現在の上流実行状態を保証しない。最新状態はTurn状態APIで再照会する。

| phase | 意味 |
| --- | --- |
| received | 保存済み・送信境界前。自動再開しない |
| dispatching | 上流送信境界を記録済み。送信・応答の途中であり結果未確定 |
| started | Turn開始応答・識別子を保存済み |
| rejected | 入力検証または明示的な上流パラメータ拒否。新規Turnは開始確認されていない |
| finished | completed / failed / interrupted を観測済み。再実行安全性は示さない |
| unknown | 送信結果・実行状態の確認ができない。Turn IDが分かる場合は状態照会可能 |

要求IDの重複POSTは、このJSON記録を返す。最終回答の再配信ではない。`GET /v1/codex/responses/{response_id}` はさらに `continuable` を返す。旧保存データは会話継続できるが、この拡張照会に必要なTurn IDを持たないため `404 response_not_found`。

`GET /v1/codex/turns/{turn_id}/approvals` の `data` に含まれる `approval_id` を使って既存承認APIから内容・期限・許可判断を取得する。承認IDの形式・連番を推測して探索しない。

なお同じAPI Keyを使うクライアント間の隔離、最終出力の再取得、イベントの履歴再配信、Steerの重複排除は提供しない。GatewayはCapabilityと制限を確認する。

## 対応結果

P-01〜P-05、P-07の制御機能とP-09の同一Provider内モデル変更を実装した。
P-06はsnapshotによる再同期・有効承認一覧・欠落通知を提供し、最終出力の再取得は非対応と回答する。
P-08は共有認証主体の範囲、要求単位の自動承認抑制とThread継続時の維持を契約化した。
添付取得・UTF-8本文の取込み・成果物返送、Discordの認可・キューはGatewayの責務。
追加cwdルート、成果物へのアクセス経路、将来の出力保存容量・期間は運用条件として残る。

模擬試験と実機試験の範囲は[検証結果](live-codex-validation.md)、開発チェックは[検証手順](development.md)を参照。
実装はソースに反映済み。サービスへの適用と更新後の確認手順は[常駐設定](server-operations.md)を参照。
