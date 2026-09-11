# ワーク・成果物・回答復旧 API v2 契約案

版: 0.2 / 2026-09-11。状態: **Gateway・Proxy間で合意済み。実装・このサーバーへの配備済み。Gateway結合・負荷などの製品受入は未完了。**

Gatewayの[調整案0.3](../../codex-hoshikage-gateway/docs/proxy-workspace-artifact-flow-proposal.ja.md)と、その後合意した4点へのProxy回答。
本書のAPIは合意した製品契約であり、容量設定値は受入確認中の候補である。実装・検証結果は[v2受入記録](v2-implementation-status.ja.md)を参照する。本書は、現行[制御API v1](control-api.ja.md)の利用手順を上書きしない。
0.2ではGatewayレビューに従い、Turn開始前の停止と、監査付きUNKNOWN占有解除を追加した。
実装量を理由に復旧要件を外さない。API形状と保証を先にレビューし、両側の要件・設計・DB・試験を揃えて実装する。

OpenAI API互換は必須の基本機能であり、`GET /v1/models`、`POST /v1/responses`、`POST /v1/chat/completions`を維持する。本書の`/v2/codex/...`はGateway向け独自拡張APIの版番号で、OpenAI互換APIの置換やサーバー全体のモード切替ではない。両方を標準で同時提供し、クライアントが用途に応じたエンドポイントを呼ぶ。互換APIの要求形式は[要件定義書](codex-hoshikage-proxy-requirements.md)を参照する。

## 1. 採用する責務と回答一覧

| ID | 回答 | 本書の具体化 |
| --- | --- | --- |
| P-W01 | 採用 | 独立した会話作成と操作ID照会。成功Responseがなくても同じ会話で次の実行が可能 |
| P-W02 | 採用 | 自動ワーク、明示共有、現行権限検証、包含ワークを含むProxy全体の実行競合制御 |
| P-A01 | 採用 | 実行に紐づく専用ツールと利用者指定が同じ成果物作成処理を利用 |
| P-A02 | 採用 | 永続操作ID、非同期作成、確定保存後の公開、再コピーしない障害回復 |
| P-A03 | 採用 | 不変のartifact ID、整合性情報、実行状態に依存しない安全な取得 |
| P-A04 | 採用 | 有限リース、容量予約、GC排他、正式復元時の世代変更 |
| P-C01 | 採用 | OpenAI互換v1を維持し、クライアントが拡張v2のエンドポイントを明示選択。実効値を機械可読で公開し、測定後に製品既定値を確定 |
| P-O01 | 採用 | 確定回答の保存・取得と配信待ちリース。実行結果と保存結果を分離 |

Proxyは会話・ワーク・実行・成果物・確定回答の正本を所有する。GatewayはDiscord認可、受付キュー、利用者向け全体2実行上限、選択UI、配信先と送信結果を所有する。
ProxyにはDiscord IDやtokenを渡さない。Gatewayはcwdを取得・保持せず、Proxyホスト上のファイルを直接開かない。
Gatewayのキャッシュ消失は再取得で復旧する。どの保存・配信失敗もAIの自動再実行を許可する根拠にしない。

## 2. 共通規則

### 2.1 認証・版・復元世代

基底パスは `/v2/codex`。全操作はBearer認証必須。現行の本人専用・共有管理主体を維持し、Discord利用者単位のマルチテナントACLを実装済みとは扱わない。
共有APIキーの保持者は運用主体として扱い、GatewayがDiscordの本人・Guild・会話・送信先を毎回検証する。
ワーク分割は通常操作での衝突防止であり、同一OSユーザーの悪意ある処理からの完全隔離ではない。

`GET /v2/codex/capabilities`以外の全要求に以下を必須とする。欠落は428、識別子または世代の不一致は409。すべてのv2応答にも現在値を付ける。

```http
X-Proxy-Instance-Id: pxy_example
X-Proxy-Recovery-Generation: gen_example
```

`instance_id`はデータ系統の不変ID、`recovery_generation`は正式復元のたび新規発行するUUID。通常再起動では変えない。
キー更新は認証主体を変えず、保存IDを引き継ぐ。別Proxyへの接続変更はGatewayの管理移行を必要とする。
絶対パス・生の上流エラー・秘密値は公開エラーや通常ログへ出さない。保護された運用ログには原因の分類と相関IDを残す。

### 2.2 操作ID・応答喪失

すべてのPOSTに `Idempotency-Key` を必須とする。形式はv1同様ASCII英数字と `-_.`、1〜128文字。認証主体内で全POSTを通じて一意。
Gatewayは外部送信前にキー・要求の照合情報をDBへ保存する。

操作の種別・対象・デフォルト解決前の正規化JSONを指紋に含める。受理時に解決した設定・IDを保存する。同じキーと同じ要求は同じ操作ID・結果IDを返し、設定変更後も再解決しない。異なる要求なら409 `idempotency_conflict`。
認証・世代検証後、重複照合を行い、同じ受理済み操作を新しい副作用として実行しない。公開結果は現在の権限で再検証する。
JSONの未知フィールドは422、欠落・不正な値は400。正規化はオブジェクトキー順を除き、文字列・配列順・数値を恣意的に同一視しない。

```http
GET /v2/codex/operations/by-key/create-conv-uuid
```

```json
{"operation_id":"op_example","kind":"conversation.create","state":"succeeded","resource":{"type":"conversation","id":"conv_example"},"error":null}
```

`GET /operations/{operation_id}`も同じ形式。操作状態は `accepted → running → succeeded | failed | unknown`。
`unknown`は証拠の照合でのみ更新し、新しい原本のコピー・上流実行を自動送信して解消しない。照会404は未実行の証明ではない。
全操作の重複防止記録は本体期限後もtombstoneとして自動削除しない。ストア容量不足は新規受付を止め、キーの再利用で回避しない。
照会・同一キー再POSTは許可するが、実行の再送は受理済み操作を返すだけ。世代不一致時は自動再POSTしない。

### 2.3 ページングと状態

一覧は `limit`（1〜100、既定50）と不透明な `cursor`。応答は `data` と `next_cursor`。
作成順の安定した境界をcursorに含め、同じ走査中の新規登録による重複・飛ばしを防ぐ。状態・期限は各ページ取得時点の値。
無効cursorは400、認可範囲または復元世代違いは409。通知は正本ではなく、再接続後は一覧・個別照会で再同期する。
時刻はUTCのRFC3339、容量はバイト、ハッシュは小文字hexのSHA-256。レスポンス例のIDは説明用短縮表記。

## 3. 会話・ワーク

### 3.1 作成と照会

```http
POST /v2/codex/conversations
Idempotency-Key: create-conv-uuid
Content-Type: application/json

{"workspace":{"mode":"automatic"},"model":"chatgpt/gpt-5.6-luna"}
```

受理時202。会話IDとworkspace IDを上流呼出し前に予約・永続化する。

```json
{"operation_id":"op_c1","state":"accepted","resource":{"type":"conversation","id":"conv_c1"}}
```

```http
GET /v2/codex/conversations/conv_c1
```

```json
{"conversation_id":"conv_c1","state":"ready","workspace_id":"ws_w1","workspace_mode":"automatic","model":"chatgpt/gpt-5.6-luna","active_response_id":null,"last_response_id":null,"execution_block":null,"artifact_registration":"available"}
```

会話作成は管理ルート内の専用ワークと永続対応まで。Codex Threadは最初の実行時に遅延作成し、会話作成自体ではAIを実行しない。
会話状態は `creating → ready | creation_failed`、管理操作による `revoked | recovery_blocked`。実行中かどうかは別フィールド。
同一操作の途中ディレクトリは所有マーカーで照合し、作成済みの同じワークだけを完成させられる。別ディレクトリを自動割当し直さない。

共有は `{"workspace":{"mode":"shared","workspace_id":"ws_shared"},"model":"..."}`。
`GET /workspaces?selectable=true`で認可済み共有候補（ID・表示名・共有の注意・状態）を返す。
`GET /workspaces/{id}`で状態を照会できるが絶対パスは返さない。共有登録・原本削除はProxy運用者の管理操作で行い、Gatewayにホストパスを登録するAPIは提供しない。
会話作成後のworkspace変更APIは設けず、変更は新規会話として明示する。会話リセットやDiscord削除は原本削除を意味しない。

### 3.2 実行先の同一性と競合

自動ワークは登録した管理ルート以下に生成する。保存物・DB・APIキー領域は実行ワークの外に置き、モデルにその書込権限を追加しない。
実行先は内部の絶対パス、所有マーカー、実体識別情報で記録する。再起動・設定変更後も現在のdefault_cwdから補完しない。
同じ文字列のパスでも実体置換・マウント変更等で同一性が確認できなければ `workspace_identity_mismatch`。正式移設手順だけで対応を更新する。
実行・登録・本体取得・保持予約ごとに現行のワーク利用許可を確認する。原本削除は保存成果物を消さないが、ワークの利用権限撤回は保存版からの取得も拒否する。

同一会話は1実行。同一または包含するワークの実行もProxy全体で排他にする。既存v1の明示cwdも同じ競合判定へ参加させる。
未知状態の実行は確認まで占有を保持する。例外は4.5節のProxy運用者による監査付き解除に限る。プロセス内Semaphoreの消失で占有を解除しない。競合は409で返し、Gatewayが永続キューで待つ。
異なるワークはProvider枠の範囲で並列可能。Gateway全体2件はGatewayの方針であり、Proxy全クライアントの総数2を意味しない。
この排他はProxyが受け付ける実行が対象。外部エディタ、直接Codex、残存バックグラウンド処理を完全に停止・排他する保証ではない。
成果物作成は実行排他を取得せず、別の有界処理枠で動く。動的ツールからの登録が自分のTurn完了を待つデッドロックを作らない。

## 4. 会話IDによる実行と回答復旧

### 4.1 永続受付の実行API

```http
POST /v2/codex/conversations/conv_c1/responses
Idempotency-Key: run-uuid
Content-Type: application/json

{"input":"集計してPDFを作ってください","model":"chatgpt/gpt-5.6-luna","metadata":{"codex.approval_capability":"interactive","codex.auto_approve_workspace":"false"}}
```

202は要求とresponse IDの永続受付。上流開始の成功を意味しない。

```json
{"operation_id":"op_r1","state":"accepted","resource":{"type":"response","id":"resp_r1"}}
```

input・reasoning・text.formatの意味はv1の対応範囲を継承。`previous_response_id`・`stream`・`codex.cwd`はこのAPIでは拒否する。
model省略はその会話で最後に開始が受理されたモデル、未開始なら会話作成時モデルを使う。同一Provider内の次Turn変更に対応。実行中モデル変更はしない。
初回の確定した開始拒否なら同じ会話・ワークで次の新規要求を許可する。Threadを作成済みならそのThreadを保持する。
初回の失敗・中断後も既知Threadを再利用できる。これは上流履歴・ファイル変更の巻戻しではない。
Thread作成／Turn開始の応答喪失は `unknown` として新規開始を拒否し、別Threadを自動作成しない。消失が確定したThreadは `conversation_unavailable` とし、新規会話を案内する。

**v2の実行はHTTP受付・監視接続から独立する。切断は中断しない。** Gatewayは停止意思を保存して4.4節の停止APIを使う。Turn IDの取得を待つ必要はない。
Proxyクラッシュ時の実行自動再送はしない。v1 Responsesの切断時中断の意味は変更しない。
受付後にProxyがまだ上流送信していない永続要求は通常処理を進められるが、送信境界を越えた要求は再送しない。
一時的な実行入力は受付直後のクラッシュに対応するためProxyで保護して永続化し、上流開始確定または確定拒否後に削除する。unknownでは照合期間中のみ保持し、削除しても重複防止記録は残す。保持期限はCapabilityで公開する。

### 4.2 実行・保存状態の分離

```http
GET /v2/codex/responses/resp_r1
```

```json
{"response_id":"resp_r1","conversation_id":"conv_c1","workspace_id":"ws_w1","phase":"finished","execution_status":"completed","last_observed_status":"completed","turn_id":"turn_t1","output":{"state":"ready","size_bytes":87,"sha256":"example-sha256","expires_at":"2026-09-18T09:00:00Z"}}
```

`phase`: `accepted → dispatching → started → finished`、開始拒否は`rejected`、上流開始を防げた取消は`cancelled`、送信・状態不確定は`unknown`。
`execution_status`: `not_started | in_progress | completed | failed | interrupted | unknown`。現在照会と最終観測は区別する。
確定回答状態: `pending | saving | ready | unavailable | failed | expired | corrupt`。実行がcompletedでも保存はfailedになり得る。
操作`response.create`のsucceededは上流開始と識別子の確定、実行完了ではない。後続状態はResponse照会が正本。開始前取消ではresponse.create操作をfailed、error.codeを`execution_cancelled`とし、Responseはphase=cancelled / execution_status=not_startedとする。実行失敗とは区別する。
失敗・中断の途中テキストを確定回答と偽らない。確定回答がなければ `unavailable` と理由を返す。

`GET /responses/{id}/events` は監視SSE。接続時 `snapshot`、以後 `response.delta`・`response.execution_terminal`・`response.output_ready`・`response.output_failed`・`artifact.ready` 等を送る。
各イベントにresponse IDを含める。通知欠落は `gap` を送って切断し、再接続・照会を促す。過去deltaの再生は保証しない。切断で実行を中断しない。
Gatewayはdeltaを暫定表示に使えるが、保存済みの確定回答で最終表示を確定する。
`response.output_ready` は本文・メタデータの永続化後に限る。実行完了だけをもって「復旧可能な回答配信完了」にしない。

```http
GET /v2/codex/responses/resp_r1/output
```

200は保存済みJSON本体（`response_id`, `model`, `output`配列）。Content-Length・Content-Digest・ETagは保存バイト列に対応。同じresponse IDでreadyとなった本文は書き換えない。
未確定は409 `output_not_ready`、保存失敗・最終出力なしは409 `output_unavailable`、期限切れ410、破損503。
復旧はProxyの完了記録・保存途中の同じバイト列、または上流の同じThread/Turnの確定結果だけで行う。別Turn生成や履歴からの推測生成をしない。
上流からも完全性を確定できなければunavailableとする。永久的なストレージ喪失まで無損失を保証しない。
ツール出力・思考過程・SSE全履歴は本保存契約の対象外。Gateway DBは本文の正本を持たず、配信用一時キャッシュは許可する。

### 4.3 制御経路

v2 Responseのturn IDを既存v1のinterrupt・steer・承認・Turn状態APIで使用できるように実装する。期待Turn照合と共有認証範囲は維持する。
v2由来の対象にはv1制御経路でも上記インスタンス・世代ヘッダーを必須にし、復元世代の検証を迂回させない。
承認・Steer・中断・照会は新規実行枠とコピー枠を取得しない。制御の具体的な成功／不明・二重回答の意味はv1を継承する。

### 4.4 Turn ID不要の停止（P-W01補完）

Gatewayはまず対象会話のキューをpauseし、未送信依頼の送信許可を失効させ、停止対象の要求キーをDBへ保存する。Proxyの停止は指定した1要求だけに作用し、後から利用者が送る別要求を永久停止する操作ではない。

```http
POST /v2/codex/stops
Idempotency-Key: stop-uuid
Content-Type: application/json

{"target":{"conversation_id":"conv_c1","request_key":"run-uuid"}}
```

Response ID判明後は `{"target":{"response_id":"resp_r1"}}` でもよい。両形式の併用・未知フィールドは拒否する。要求キーは元のresponse.create要求のキーであり、この停止POSTのキーとは別。
応答は次の形式。停止意思が永続化されたことと、実際に停止したことを分ける。

```json
{"operation_id":"op_s1","stop_id":"stop_s1","response_id":"resp_r1","intent_state":"recorded","stop_status":"waiting_for_start","execution_status":"unknown","interrupt_delivery":"not_sent"}
```

`GET /stops/{stop_id}`で同じ形式を照会できる。操作ID・停止キーからも2.2節に従って照会可能。
stop操作のsucceededは停止意思の永続記録を意味し、中断完了ではない。`stop_status`とResponse照会で実際の結果を確認する。

| 受付時の状態 | Proxyの処理 | HTTP / stop_status |
| --- | --- | --- |
| 元要求未到着（キー指定） | 会話と要求キーの取消予約を同期保存し、遅着する元要求の実行を禁止 | 200 / cancelled_before_start |
| 受理済み・上流開始前 | dispatchと排他的に取消をcommit。入力と予約枠を解放し、開始を禁止 | 200 / cancelled_before_start |
| Thread作成中・Turn開始中 | 停止意思を保存。次の上流開始境界を禁止し、既に送ったTurnの識別確定を待つ | 202 / waiting_for_start |
| 実行中・Turn判明 | 対象Turnだけへinterrupt。RPC受付は完了の証明ではない | 202 / interrupt_pending |
| 実行の終端が確認済み | completed / failed / interruptedをそのまま返す。後付けの停止成功にしない | 200 / already_terminal |
| 開始・実行・中断結果が不明 | 停止意思を残して照合。対象不明のまま別Turnを中断しない | 202 / unknown |

後から中断が確認できた場合は `stop_status=interrupted`。先に自然完了・失敗した場合は `already_terminal`と実際のexecution_statusを返す。`cancelled_before_start`はTurnが開始しなかったこと、`interrupted`は対象Turnの中断確認であり、ファイル変更の巻戻し・全子孫プロセスの停止は意味しない。
Response ID指定で対象がなければ404 `resource_not_found`。元キーが別種の操作・別会話に属すれば409 `target_mismatch`。認可された存在する会話についてのみ未到着キーの取消予約を認める。
取消予約は重複防止記録と同じ寿命で保持する。元要求が後着した際はキーを再利用可能にせず、要求を照合してcancelledなResponseと操作結果へ結び付ける。予約自体を「過去に実行されなかった証拠」として復元世代を越えて流用しない。

dispatchの送信権獲得と停止commitを同じ会話・要求の整合境界で直列化する。停止が先なら上流開始を送らない。送信権獲得が先なら開始中として扱い、送信したか判別不能な窓を「開始前取消」と報告しない。
thread/start、thread/resume、turn/startそれぞれの前に停止を検査する。Thread作成だけ終わった場合はワーク・Thread対応を保持し、Turnを開始せず取消を確定できる。通信中にDBロックを保持して停止APIを待たせない。
Turnが判明した後は停止意思を確認して対象IDへ中断する。遅れて到着した開始応答・再起動後の照合でも同じ規則とし、別の新規実行に停止対象を付け替えない。
`interrupt_delivery`は `not_sent | dispatching | accepted | rejected | unknown`。RPC送信前に送信境界を保存し、応答喪失後に無条件再送しない。明示的な開始直後の拒否だけはv1の限定再試行規則を継承する。

同じ停止キーは同じstop ID、別停止キーで同じ実行を指定した場合も実行単位の停止意思・中断送信状態を共有し、重複RPCを発生させない。状態変化は照会・snapshotで取得する。停止後のsteerは409で拒否する。
停止記録の保存失敗は503であり、停止を受け付けたと返さない。復元保留中も現世代・対象を検証した停止操作は許可し、新規実行は許可しない。
停止・停止照会は実行枠・コピー枠を使わない。Gatewayのpause解除はProxyの停止意思を取り消さず、新しい依頼には新しい要求キーを使う。

### 4.5 UNKNOWN占有の監査付き解除（P-W02補完）

解消不能なunknownを残したままワークを再利用するため、Proxyホストの運用者だけが使う管理CLIを提供する。**通常Bearer APIとGatewayの操作には解除権限を与えない。** Gatewayは照会結果を案内し、自身のhold解除でProxyを迂回しない。
以下は新設予定CLIの契約例であり、現在実行できるコマンドではない。

```sh
codex-hoshikage-proxy admin execution-hold inspect --response-id resp_r1
codex-hoshikage-proxy admin execution-hold release --response-id resp_r1 --expected-revision 42 --review-token review_example --operation-id release-uuid --reason '上流記録を照合したが解消不能。競合リスクを確認した' --accept-risk
```

inspectはinstance・世代、対象Response／要求キー、会話・workspace・包含競合範囲、Thread／Turn、最終観測、停止意思・中断送信状態、占有revision、残るリスクを表示し、5分有効なreview tokenを返す。
releaseは同じOS運用主体・対象・世代・revisionを検証し、理由と明示risk承認を必須にする。tokenは1対象・1操作に拘束し、期限切れ・状態変化・世代変更では再inspectを要求する。同じoperation IDの再実行は保存済みの結果を返す。
稼働Proxyの私有管理socketを通じて通常dispatchと同じ排他・transactionを使用する。DBを外から直接書き換えない。サービス停止中は直接編集で代替せず、管理モードで同じストア専有lockを取得して処理する。

解除条件は、対象がなおunknownで、照会による通常解決ができず、指定した占有が対象要求に属すること。実行中と確認できた対象の解除は `execution_not_unknown`で拒否する。終端が確認できれば監査付き例外ではなく通常の占有解放を行う。
解除は次を同一transactionでcommitする。失敗時は占有を維持し、監査なしの解除を成功扱いしない。

- 対象の履歴・実行結果unknown・重複防止キー・停止意思を保持する。`dispatch_eligible=false`を永久に設定し、元要求の新規送信・再実行を禁止する。既知対象への停止照合は継続できる。
- この要求由来の会話・ワーク・Provider占有だけを `administratively_released` とし、他の要求の占有、Gatewayのpause、復元全体の保留を解除しない。
- 監査ID、operation ID、OS主体、理由、時刻、世代、照合revision、対象範囲、解除前後の状態、承認したリスクを永続保存する。

```json
{"audit_id":"audit_h1","response_id":"resp_r1","execution_status":"unknown","hold_state":"administratively_released","dispatch_eligible":false,"conversation_state":"recovery_blocked","workspace_reuse":"explicit_new_conversation"}
```

**旧Threadへ未確認のTurnを重ねない。** 解除対象の会話はrecovery_blockedのまま隔離し、同じワークを使う明示的新規会話を許可する。自動ワークの再利用許可も管理操作に紐づけて提示し、GatewayはID・表示名で選ぶ。原本・旧履歴は移動・削除しない。旧会話が後に安全と確認できた場合だけ、別の管理確認で隔離解除できる。
占有解除は残存実行や外部プロセスが止まった証明ではない。例外適用後は実際の並列数・共有ファイルへの競合が通常保証を超え得ることを運用者に表示する。

Response照会に `hold_state`, `hold_revision`, `dispatch_eligible`, `administrative_release`（監査ID・時刻のみ）を追加する。会話／ワーク照会には再利用可否と理由を返す。運用理由の生文はDiscordへ公開しない。
解除後も識別可能な対象を低頻度で再照会する。遅着通知・再照会で実行中と判明したら、対象占有を復元し、同一・包含ワークの新規開始を禁止する。Providerの占有も再計上し、上限超過時はそのProviderの新規開始を保留する。
既に始まった別要求を勝手に停止しない。元要求の停止意思があれば、確認できた元Turnに限り中断処理を行う。再検出を監査・状態照会・通知へ反映し、Gatewayは通知欠落後も照会で検出する。
遅着した終端は正しいResponseの観測を更新し、別実行の占有やモデル選択を上書きしない。解除と遅着・新規dispatchは同じ競合境界で順序を確定し、古い観測で新しいholdを消さない。

## 5. 成果物の作成・登録

### 5.1 利用者指定

```http
POST /v2/codex/conversations/conv_c1/artifacts
Idempotency-Key: capture-uuid
Content-Type: application/json

{"path":"output/report.pdf","display_name":"集計レポート.pdf","response_id":"resp_r1"}
```

202で操作ID・予約artifact IDを返す。

```json
{"operation_id":"op_a1","state":"accepted","resource":{"type":"artifact","id":"art_a1"}}
```

response IDは任意だが、指定時は同じ会話への所属を必ず検証する。
実行のないreadyな会話でも作成できる。記録のないResponseを指定した要求を会話だけの要求へ黙って読み替えない。
pathはUTF-8の相対パス。絶対パス、空要素、`.`・`..`、NUL、バックスラッシュを拒否し、デコードを重ねて別パスに変換しない。
通常ファイルのみ。ディレクトリ・symlink・特殊ファイル、ワーク境界を越えるマウント・参照を拒否する。
Linuxではディレクトリfdを起点とする安全な解決・openを使い、事前canonicalizeだけに依存しない。hard link等の別名を通じた範囲外取得にも対処し、初期方針は複数リンクのファイルを拒否する。

同じfdから上限付きコピーを作り、読取り前後の実体・サイズ・mtime・ctimeと実読取り量を検査する。検出した変更は409 `source_changed`で公開しない。
コピー途中の全変更検出・原本の単一時点snapshotは保証しない。完成登録の通常経路ではモデルにファイルを閉じてから呼ぶよう指示する。
表示名はパスに使用せず、制御文字・区切り等を拒否する。メディア型はProxyが決定し、判定不能は `application/octet-stream`。モデルの申告をHTTPヘッダーへ無検証転記しない。

### 5.2 成果物状態と作成の永続化

作成操作: `accepted → running → succeeded | failed | unknown`。
成果物保存状態: `creating → ready | failed | unknown`、readyから `expired | corrupt`。アクセス状態は別の `allowed | revoked` とし、一時的な権限撤回を保存物消失と混同しない。
一覧用の有効状態は権限検証後に合成する。保持が残っていてもrevokedなら本体を返さない。

本体を私有stagingへ書く → fsync → サイズ・ハッシュ確認 → 不変の保存名へrename・親同期 → DBでreadyとメタデータをcommit → 成功公開。
操作・資源・容量予約を同じDB transactionで扱う。ファイルとDBの原子性を仮定せず、操作IDに紐づく完成マニフェストを使ってクラッシュ後に照合する。
完成済みの同じ保存物と検証情報を確認できれば公開を完了できる。部分コピーは失敗／不明として記録し、原本を読み直して同じartifact IDを完成させない。
READYなのに本体がない・ハッシュ不一致ならcorrupt。原本から補修せず、同じ正本のバックアップでのみ復旧する。

### 5.3 実行側の専用ツール

提案ツール名は `hoshikage_publish_artifact`。入力は以下だけを許可する。

```json
{"path":"output/report.pdf","display_name":"集計レポート.pdf"}
```

Proxyが認証済みApp Server接続のthread ID・turn ID・tool call IDから会話・ワーク・作成キーを決定する。モデルが会話ID・workspace ID・任意URL・認証情報を指定する余地を設けない。
同じtool call IDは同じ作成操作、別tool callは別版となる。登録は送信許可ではない。
ツール成功は保存物がreadyになった後だけ返す。有限の待機期限を超えた場合は `pending` と操作IDを返し、公開成功とは扱わない。処理は照会可能に残し、モデルへ重複登録を促さない。
Gatewayの一覧・状態照会はツール応答の到達に依存せず、後からreadyになった成果物も検出できる。

上流の採用候補は `thread/start.dynamicTools` と `item/tool/call`。公式には実験的APIであり、実装時に対応版を固定してadapter内へ閉じ込める。
公式説明は[Codex App Server](https://developers.openai.com/codex/app-server/)のDynamic tool calls節を参照。2026-09-11にローカルCodex 0.153.4のexperimental schemaでもフィールド・server requestの存在を確認した。これらは実接続成功の証拠ではない。
新規Thread・再開・モデル変更後の登録、早期tool call、キャンセル、重複RPCを実モデルで受入試験する。未対応モデルでは `artifact_registration=unavailable` と明示し、製品標準フローの受入済みモデルに数えない。
旧Threadに専用ツールを確実に提供できない場合、同じワークを使う新規会話への明示移行を案内する。過去履歴を無断で別Threadへコピーしない。

## 6. 成果物一覧・取得

```http
GET /v2/codex/conversations/conv_c1/artifacts?limit=50
```

```json
{"data":[{"artifact_id":"art_a1","conversation_id":"conv_c1","workspace_id":"ws_w1","response_id":"resp_r1","display_name":"集計レポート.pdf","version":1,"state":"ready","size_bytes":1048576,"media_type":"application/pdf","sha256":"example-sha256","created_at":"2026-09-11T09:00:00Z","expires_at":"2026-09-18T09:00:00Z"}],"next_cursor":null}
```

会話の登録物だけを返す。共有全体は `GET /workspaces/{id}/artifacts` で明示選択する。原本の走査結果やモデル回答中のパスを一覧へ混入させない。
版番号は会話＋正規化した元相対パス単位の単調増加値。失敗で欠番可。ハッシュが同じでも別操作の版を勝手に統合しない。
`GET /artifacts/{id}` は状態・メタデータ、`GET /artifacts/{id}/content` は不変の本体を返す。
本体はContent-Length、強いETag、`Content-Digest: sha-256=:base64:`、安全なContent-Disposition、`Cache-Control: private, no-store`、`X-Content-Type-Options: nosniff`を付ける。
単一byte RangeとIf-Rangeに対応（206/416）。配信再開は保存済みartifact IDと検証情報に対して行い、元のpathへフォールバックしない。
期限切れ後のメタデータはtombstoneとして残し、本体取得は410。保存時の正本に対する検証失敗は503 `content_corrupt`。完了前の切断をGatewayは成功扱いしない。
ダウンロード中のGC防止はProxyが内部読取参照として確保する。既に配信したバイト列の回収や、取得済みキャッシュへの権限撤回の強制は保証できない。

## 7. 成果物・回答の保持予約

```http
POST /v2/codex/leases
Idempotency-Key: reserve-delivery-uuid
Content-Type: application/json

{"resource":{"type":"artifact","id":"art_a1"},"hold_until":"2026-09-12T09:00:00Z"}
```

回答は `{"type":"response_output","id":"resp_r1"}`。対象はreadyで、現在取得可能なもののみ。
成功201は `operation_id`, `lease_id`, `resource`, `hold_until`, `max_hold_until`, `state=active` を返す。同期完了したリース操作も操作照会へsucceededとして記録する。

```json
{"operation_id":"op_l1","lease_id":"lease_l1","resource":{"type":"artifact","id":"art_a1"},"hold_until":"2026-09-12T09:00:00Z","max_hold_until":"2026-10-11T09:00:00Z","state":"active"}
```

要求期限を満たせない場合は短縮成功せず409 `retention_limit`。
`GET /leases/{id}`、`POST /leases/{id}/extend`（同形式のhold_until）、`POST /leases/{id}/release`（空JSON）を提供する。
延長・解放にも別Idempotency-Keyを使用し、同じ操作は同じ結果。解放済みの再解放は200、解放後の延長は409。期限を過ぎたリースを復活させない。

保持期限は `max(ready時の基本期限, 有効リースのhold_until)`。リース解放は即時削除を意味せず、基本期限までは取得できる。
作成時からの最大寿命を超える延長は拒否し、別リースを作って上限を迂回できないようにする。権限撤回はリースより優先する。
リース取得・GC判定・読取り開始は同じ整合境界で判定する。期限切れ判定を先に確定した対象へリースを付け直さない。
GCは回収対象を記録してから本体を削除する。切断・失敗した取得の内部参照は有限timeoutで解放し、Gatewayの永続リースとは区別する。
Gatewayは配信待ちを保存→リース取得・期限保存→本体取得→送信意思保存→Discord送信→結果保存の順に処理する。結果不明を自動再送で解消しない。

## 8. 制限・既定値・容量不足

以下は**測定前の初期候補値**。保証された性能値や採用済み製品既定値ではない。実装時の対象機での測定・Gatewayレビュー後に設定例と同時確定する。

| 設定 | 候補 | 意味 |
| --- | --- | --- |
| artifact_max_bytes | 268435456（256 MiB） | 1成果物上限。Discordの上限とは別 |
| artifact_store_max_bytes | 8589934592（8 GiB） | 保存物＋予約容量の上限 |
| capture_concurrency | 2 | コピー処理枠。実行2並列とは独立 |
| capture_timeout_seconds | 120 | 同じ原本からの1回のコピー期限 |
| download_concurrency | 4 | 本体配信枠。制御APIは別枠 |
| download_timeout_seconds | 300 | 1ダウンロードの最大時間 |
| artifact_retention_seconds | 604800（7日） | readyからの基本期限 |
| lease_max_lifetime_seconds | 2592000（30日） | readyからの延長可能な絶対上限 |
| execution_input_max_bytes | 16777216（16 MiB） | 1実行の保存対象入力上限（画像data URL等を含む） |
| execution_input_store_max_bytes | 268435456（256 MiB） | 受付入力の保存・予約枠 |
| output_max_bytes | 8388608（8 MiB） | 保存する確定回答1件の上限 |
| output_store_max_bytes | 1073741824（1 GiB） | 回答専用の保存・予約枠 |
| output_retention_seconds | 604800（7日） | 回答基本期限。リースも同じ仕組み |
| unresolved_input_retention_seconds | 86400（1日） | 未解決入力の保持上限。期限後に自動送信しない |
| disk_free_floor_bytes | 2147483648（2 GiB） | 新規予約後にも残すディスク余裕 |

予約は作成受付時に確保し、既存リースを破って空きを作らない。stagingと公開本体が同一filesystemでrenameできる配置を必須にし、二重コピーの容量仮定を避ける。
サイズ不明・伸長ファイルも実バイト上限で止める。実行受付時には入力のサイズ上限・保持枠も検証して予約し、未送信のまま入力期限が切れた場合はrejectedとし送信しない。確定回答の最大保存枠を予約し、不足ならAI開始前に拒否する。
それでも保存障害・上限超過は発生し得るため、実行結果と保存失敗を分離する。原本・メタデータ・重複防止記録もディスクを消費し、成果物上限だけで全体を保護できるとはしない。
制限引下げは新規受付へ適用し、既存の返した保持期限を短縮しない。既に予約超過なら新規受付を止め、状態と整理方法を案内する。
ワーク原本は自動削除しない。容量と所有会話を管理画面／管理CLIで確認し、明示整理する。
測定項目は実行2件＋コピー＋配信時の制御応答、メモリ、空きディスク、最大サイズの所要時間、再起動回収、低速別ホスト。結果に基づいて候補値を修正する。

## 9. Capabilityとエラー

```http
GET /v2/codex/capabilities
```

```json
{"contract_version":"2.0","instance_id":"pxy_example","recovery_generation":"gen_example","recovery_state":"ready","features":{"managed_conversations":true,"durable_execution":true,"stop_by_request":true,"stop_before_acceptance":true,"administrative_hold_release":"local_operator","workspace_selection":true,"artifact_capture":true,"artifact_registration_tool":true,"artifact_listing":true,"artifact_range_download":true,"retention_leases":true,"response_output_retrieval":true},"limits":{"auth_scope":"shared_operator","workspace_isolation":"operational_separation","execution_disconnect_interrupts":false,"event_replay":false,"artifact_max_bytes":268435456,"artifact_retention_seconds":604800,"lease_max_lifetime_seconds":2592000},"registration_models":["chatgpt/gpt-5.6-luna"]}
```

これは将来の応答例。実応答には8節の全実効制限と `operations_retention=until_explicit_state_retirement`、`server_time` を含める。model一覧は実際に受入済みのものだけを公開する。
`GET /capacity` は予約済み・使用済み・空き容量と新規受付可否を返す。変動するため、照会成功だけで後続受付を保証しない。

```json
{"error":{"code":"source_changed","message":"ファイルの更新を検出しました。新しい取得操作を開始してください。","retry":{"action":"new_operation","after_seconds":2},"operation_id":"op_a1"}}
```

| HTTP | code例 | Gatewayの扱い |
| --- | --- | --- |
| 400/422 | invalid_argument / unknown_field | 入力を訂正。新しい操作なら新キー |
| 401 | invalid_api_key | 接続設定の確認。生の認証情報を表示しない |
| 403 | workspace_access_revoked / resource_access_denied | 許可撤回を案内。別IDで迂回しない |
| 404 | operation_not_found / resource_not_found / source_not_found | 不明IDと原本なしを区別。自動実行しない |
| 409 | idempotency_conflict / target_mismatch | 対応誤りを解消。黙って別対象を選ばない |
| 409 | execution_not_unknown / hold_revision_conflict / review_token_expired | 管理状態を再照会。古い確認で解除しない |
| 409 | conversation_busy / workspace_busy / execution_unknown | 状態照会。終端確定まで新規実行を保留 |
| 409 | workspace_unknown / workspace_identity_mismatch / conversation_unavailable | 管理確認または明示的新規会話 |
| 409 | source_changed / output_not_ready / output_unavailable | 更新中・未保存・取得不能を区別 |
| 409 | retention_limit / instance_mismatch / recovery_generation_mismatch | リース条件見直しまたは復元照合 |
| 410 | content_expired / lease_expired | 同じ版は期限切れ。新規取得と再送を区別 |
| 413 | artifact_too_large / output_too_large | 上限表示。AI自動再実行なし |
| 416 | range_not_satisfiable | 同じ保存版のサイズ・Rangeを確認 |
| 428 | instance_precondition_required | 必須識別ヘッダーを付ける |
| 429 | capture_capacity_busy / download_capacity_busy | Retry-After後に照会／受付再試行 |
| 503 | store_unavailable / content_corrupt / recovery_blocked | 復旧待ち。生成・コピーで補修しない |
| 507 | storage_capacity_exceeded | 容量整理または設定確認 |

`retry.action`は `none | poll_operation | repeat_same_request | new_operation | operator_action`。HTTPコードだけで新キー再実行を判断しない。
受理後の非同期失敗はGETの200の操作オブジェクト内に同じerrorを返す。本体応答の途中失敗はJSONへ切替えず接続を終了し、Gatewayが長さ・ハッシュで失敗検知する。

## 10. 永続化・バックアップ・復元

新しい会話・操作・実行・リース・成果物・回答メタデータはtransaction可能なDBへ統合する案を採用する。SQLite WAL＋同期commitを基本とし、ファイル公開との整合は5節のマニフェストで扱う。
現行JSONLからの移行は退避・検証・重複防止キーの保存を伴う一度の処理。現行バイナリが新DBを無視して起動できないschema/versionガードを運用手順にも設ける。
ストアを同時に複数Proxyが所有しない。鍵・データ・tempの所有権を検証し、ログに本文を出さない。保存時暗号化の有無は運用設定として明示し、未実装の暗号化を保証しない。

正式バックアップは受付を止め、活動実行とコピーを確認してから、DB・保存物・manifest・ワーク対応・必要なCodex会話データを整合したbundleにする。
ワーク原本は別の保全対象としてmanifestに含む／含まないを表示する。外部書込みがある原本の整合を無条件に保証しない。

正式復元手順:

1. ProxyとGatewayの新規受付を止め、Proxyサービスを停止する。旧子プロセスが残っていないことを確認する。
2. 復元対象の外にrestore-pendingマーカーを同期保存し、新しいrecovery_generationを生成する。
3. manifestとハッシュを検証し、DB・保存物を復元する。欠損は成功扱いせず、対象corruptまたは復元保留にする。
4. 復元DBへ新世代と `recovery_blocked` を保存する。旧未終端実行をunknownとし、自動送信しない。期限は元の絶対時刻を維持し、復元で延長しない。
5. 認証付きCapability・対象照会、および4.4節の対象限定停止だけを有効にする。Gatewayは世代不一致を検出し、DBの未完了操作・Response・artifact・leaseを新世代と照合する。存在しない旧操作を新規受付へ戻さない。
6. 運用者が照合結果と残る不明対象を確認して復元保留を解除する。新世代をGatewayに保存し、以後の新規依頼だけを通常受付に戻す。個別unknownの占有は通常照合または4.5節の監査付き解除まで維持する。

復元保留解除は「古い全要求が未実行だった」という証明ではない。Gateway側の既存restore隔離も解除条件を別に維持する。
検出対象は正式手順による復元・既知インスタンス不一致・整合検査で分かる欠損。識別情報を含むディスク全体の任意巻戻しを必ず検出する保証はない。
本体期限切れと保存破損・世代不一致を区別する。artifact/response IDを別の内容へ再割当しない。

## 11. 既存クライアントと移行

既存 `/v1/responses`・Chat・制御APIの形式は維持する。v1の新規会話を無断で自動ワークへ切り替えない。v1の完了前切断は従来どおり中断契機。
v1の継続cwdは保存済み実行先の継承へ修正し、明示した異なるcwdは409にする。この挙動修正と同一ワーク競合拒否はリリースノート・移行試験へ明記する。
旧記録のcwdを信頼できる根拠で確定できなければ、明示的な移行対象として止める。default_cwdから補完しない。
既存の外部ワークを自動ディレクトリへ移動せず、管理操作で既存ワークとして登録する。汎用HTTPで任意cwd登録を開放しない。
Gateway標準はv2の会話作成・永続実行・成果物・回答復旧を使用する。Capability不足時は利用者に機能不足を示し、v1へ黙ってフォールバックしない。
現在のサービス設定・許可ルート・sandboxは本書作成だけでは変更しない。管理ルートと保存容量の具体設定は実装・検証後に一つの導入手順として反映する。

## 12. 受入条件とGatewayへのレビュー依頼

自動テスト、Fake App Serverによる障害注入、対象Codex・モデルの実接続、Discordと別ホストGatewayを含む結合試験を区別して記録する。

| 対象 | 必須試験 |
| --- | --- |
| 会話作成 | 同じキー並列要求、受理直後クラッシュ、ワーク作成後応答喪失、既定ルート変更、初回確定拒否後の継続 |
| 実行 | 初回失敗・中断後の同じ会話、開始RPC応答喪失、同一ワークと包含cwdのv1/v2競合、v2監視切断と明示停止 |
| 開始前停止 | 停止が元要求より先着、202応答喪失、dispatch権獲得との同時commit、Thread作成中・Turn開始中の停止、停止commit直後クラッシュ。開始しないか、正しいTurnへ停止意思を適用し、取消と不明を区別 |
| 停止照合 | 同一／別停止キーの重複、interrupt応答喪失、停止と自然完了、停止後steer、再起動・遅着開始応答、復元世代違い。別Turnを中断せず停止成功を推測しない |
| UNKNOWN解除 | 古いrevision・token・世代、実行中への誤解除、監査保存失敗、解除応答喪失と再実行、複数の包含hold、解除後の元要求再POST。記録を残し対象holdだけ解除 |
| 解除後再検出 | 遅着in-progressと新規dispatchの競合、Provider上限超過、別要求実行中の再検出、遅着終端、Proxy/Gateway再起動。占有を再計上し、別要求を停止・解放しない |
| 動的ツール | 新規・再開・モデル変更、開始応答前のtool call、重複・遅延・Turn中断、コピー枠飽和でも制御可能 |
| 成果物作成 | 全永続化境界のクラッシュ、部分コピー、原本差し替え・hard link・symlink・特殊ファイル、容量不足、実行中取得 |
| 取得 | 原本更新・削除後も同じID/ハッシュ、Range再開、欠損・破損、権限撤回、会話／共有一覧の区別 |
| 保持 | 境界時刻のリース・GC競合、解放再送、延長上限、設定引下げ、切断参照回収、時計変更時の期限判定 |
| 回答 | 保存前クラッシュ、保存失敗、確定イベントと本文照合、上限超過、Gateway再起動後の同一回答・リース |
| 復元 | DBのみ／本体のみの欠損、旧Gateway DBとの不一致、復元途中クラッシュ、期限維持、自動再送禁止 |
| 配信 | Discord送信応答喪失、本人権限撤回、temp消失、同版再送と新版取得、別ホストと低速回線 |
| 性能 | 2実行＋コピー＋配信の飽和下で承認・中断・照会が枠待ちせず処理される。制御応答の測定値を公開 |

Gatewayには特に、(1)独立会話＋202永続実行への切替、(2)v2の切断非中断と明示停止、(3)成果物／回答の共通リース、(4)世代不一致時の受付保留、(5)操作IDごとの再照会規則、(6)キー指定停止の先着取消予約、(7)監査付き解除後は旧会話を隔離して同じワークを明示再利用する操作を照合してほしい。
容量・保持の数値は8節の測定前候補としてレビューし、実環境測定結果とともに最終固定する。数値未測定をAPI実装済み・受入済みと扱わない。
