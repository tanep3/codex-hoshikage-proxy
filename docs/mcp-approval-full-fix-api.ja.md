# MCP承認全体是正 API — 0.5接続合意版

> 2026-09-17 責務再整理中：[汎用基盤とポリシー拡張の改訂](mcp-approval-layering-revision.ja.md)を参照。全ツールの意味解析を汎用単発承認の前提とする記述、および追加禁止を標準API全体へ適用する記述は見直し対象。本書をそのまま新規実装へ使わない。既存0.5 wireの後継差分は未確定であり、合意済みとして扱わない。

2026-09-17。**接続仕様はGatewayと合意済み。内部詳細設計は継続中、未実装。** Gatewayの全体是正依頼への回答。通常3形式だけの修正を先行配備しない。要件・カタログ・全ツールの表示／許可・Gatewayの表示と復旧を一体で是正する。

接続仕様は本書、ツール別の意味・引数は[表示設計](mcp-approval-projection-design.ja.md)と[342ツール台帳](mcp-approval-tool-coverage.ja.md)、実証と未完了項目は[検証記録](mcp-approval-full-fix-validation.ja.md)。**台帳の詳細設計が残る行は実装開始条件を満たさない。本書の提示だけで全体設計完成とはしない。**

## 1. 利用者が見る画面と確定した方針

| 操作 | 最初のカード | 許可の選択 |
| --- | --- | --- |
| 通常の非機密な操作 | 操作内容・対象・入力内容・条件を表示 | 今回だけ許可／この依頼中、このツールを許可／拒否 |
| 非機密の外部送信・共有権限変更・削除 | 操作内容を表示 | 今回だけ許可／拒否 |
| 個人情報・連絡先・DM／メール・認証・任意実行コードを含む操作 | 値を出さず「自分だけに表示して確認／拒否」 | 依頼した本人だけに見えるメッセージに内容と実際の許可・拒否を表示 |
| ツール定義の取得中・通信失敗 | 「操作内容を取得中」「取得できませんでした」等、理由を表示 | 再確認／拒否。許可しない |

利用者は、本人だけに見えるメッセージには同じチャンネルの他の参加者から内容が見えないことを確認し、上表の導線を了承した。ボタンを開いただけでは実行しない。DMへの移動は追加しない。通常の非機密入力・投稿文・編集本文・フォーム入力値は全文表示してよい。個人情報等と判定した場合は呼出し全体を本人向け表示へ分ける。

外部送信・共有権限変更・削除・任意実行コード・秘密入力・認証は毎回確認。それ以外の評価済み通常操作は依頼中許可を提供する。公開可否と許可範囲は別であり、メール読取は本人向け表示の中で依頼中許可が可能、メール送信は単発のみ。常時許可される上流ツールに新たな承認要求を作らない。

## 2. 責務・互換性

Proxyは実callの特定、カタログ、操作の説明、公開可否、許可の条件・適用・失効を担当する。Gatewayは本人と会話の認可、表示、配信した版の保存、押下照合、再起動後の照会を担当する。Gatewayに342ツールの意味や秘密検査を重複実装させない。コマンド／ファイル変更承認G-01はGatewayが別途是正し、説明を省略したまま許可を出さない原則を共有する。

0.3/0.4を残し、0.5はResponse単位で選ぶ。既存Responseは自動変更しない。標準OpenAI互換APIは変更しない。追加の利用者向けON/OFFスイッチは設けず、既存mcp_turn_approval_enabledを使う。

capabilitiesの既存mcp_inline_approvalは値も形も維持する。別のトップレベルキーを追加する。

```json
{
  "mcp_approval_presentation": {
    "enabled": true,
    "profile": "source-conversation-v2",
    "renderer": "evaluated-operation-v1",
    "max_response_bytes": 65536,
    "max_display_text_utf16_units": 4800,
    "max_display_fields": 24,
    "max_presentations_per_interaction_per_audience": 4,
    "max_get_wait_ms": 250,
    "retry_after_ms": 2000,
    "private_details": true,
    "max_private_pages": 64,
    "max_private_display_bytes": 262144,
    "turn_policy": "evaluated-normal-operations-v1"
  }
}
```

このobjectは全フィールド必須、未知profile／renderer／policyは未対応として扱う。旧capabilityを上書きして旧Gatewayを突然不動にしない。新Gatewayは新objectの完全検証後だけ0.5を選ぶ。未知capabilityがあっても既存objectの解釈は変えない。

Response受付で次を指定する。

```json
{
  "input":"依頼本文",
  "interaction_capabilities":["mcp_form"],
  "approval_context":{"principal_id":"opaque-user","channel_id":"opaque-thread","run_id":"run-123"},
  "approval_presentation":{"mode":"source_conversation","profile":"source-conversation-v2"}
}
```

modeだけなら従来0.4、全体省略なら従来0.3。profileは上記固定値だけを追加し、余計なキー・不正型・profileなしcontext不正は400 invalid_approval_presentation。未実装／無効の新profileは503 approval_presentation_unavailable。Gatewayは失敗した実行受付を新キーで勝手に再送しない。既存の同一キーoperation照会は先に行う。宣言はResponseで不変、次Responseへ継承しない。

## 3. 表示の取得

既存エンドポイントを使う。

- `GET /v2/codex/interactions/{id}/presentation`：元会話に表示してよい内容。
- `GET /v2/codex/interactions/{id}/presentation?audience=requester`：押した本人だけに見せる内容。0.5を宣言したResponse専用。

既存API認証・instance／復元世代・workspace認可を継承。requesterを指定しても本人確認を省略できる意味ではない。Gatewayが押下者をResponseのprincipal_idへ照合した後だけ取得し、本人だけに見えるメッセージとして送信する。共有API keyだけでDiscordの閲覧者本人をProxyが識別できるとは扱わない。

### 通常操作の応答例（全トップレベルフィールド）

```json
{
  "interaction_id":"int_123","response_id":"resp_123","turn_id":"turn_123",
  "revision":1,"scope_fingerprint":"opaque-scope",
  "scope":{"instance_id":"pxy_example","recovery_generation":"recovery_example","context":{"principal_id":"opaque-user","channel_id":"opaque-thread","run_id":"run-123"},"response_id":"resp_123","conversation_id":"conv_123","workspace_id":"ws_123","turn_id":"turn_123","input_generation":0,"config_generation":"config-opaque","server":"playwright","tool":"browser_tabs","policy_generation":"policy-opaque","definition_generation":"definition-opaque"},
  "presentation_id":"present_123","presentation_fingerprint":"opaque-presentation",
  "profile":"source-conversation-v2","renderer":"evaluated-operation-v1",
  "audience":{"kind":"source_conversation","channel_id":"opaque-thread","principal_id":null},
  "state":"ready","reason":null,"expires_at":"2026-09-17T12:10:00Z",
  "diagnostic":{"code":null,"retryable":false,"retry_after_ms":null},
  "page":{"index":0,"count":1,"token":"opaque-page","content_fingerprint":"opaque-full-display"},
  "tool_policy":{
    "policy_id":"evaluated-normal-operations-v1",
    "policy_generation":"policy-opaque","definition_generation":"definition-opaque",
    "effects":["read"],"turn_eligible":true,"reason":null,
    "grant_scope":"turn_tool","eligible_operations":["list","new","select"],
    "always_confirm_operations":["close"]
  },
  "display":{
    "disclosure":"source_conversation","provenance":"proxy_verified_call",
    "title":"ブラウザーのタブ一覧を確認します",
    "fields":[{"label":"操作","value":"タブ一覧を読み取ります"},{"label":"対象","value":"このMCP接続のブラウザー"}],
    "limitations":["依頼中の許可は一覧取得・新規タブ・タブ選択に使います。タブを閉じる場合は毎回確認します","許可はこの依頼の間だけ有効です。停止・追加発言・終了で失効します"],
    "omissions":[]
  },
  "actions":{"allow_once":true,"allow_turn_tool":true,"decline":true,"open_private_details":false,"retry":false}
}
```

### 型・表示先

0.4と同じID長8192 UTF-8 bytes、revisionは1以上、channel／principalは最大128 bytes。scope_fingerprintはcallと全実引数・世代へ拘束した不透明値。reasonとdiagnostic.codeは同値または双方null。世代値をカード本文へ転載しない。

audienceはkind/channel_id/principal_idの3項目必須。source_conversationではprincipal_id=null、requesterではResponseのprincipal_id、channel_idはいずれも元会話。display.disclosureはそれぞれsource_conversation/requester_only。Gatewayは両者の一致を検証する。requesterの本文を公開メッセージへ流用しない。

rendererは評価済みの説明を生成できるときevaluated-operation-v1、できないときnull。Gatewayはprofileとこの固定表示形式、構造と完全性を検証する。tool名ごとのrenderer whitelistを別途増やす方式にしない。実ツールの定義照合と意味の評価はProxyの登録表で行う。

### 状態とactions（pending時）

| state | allow_once | allow_turn_tool | decline | open_private_details | retry |
| --- | --- | --- | --- | --- | --- |
| ready | true | 現在の適格性 | true | false | false |
| private_required（元会話用） | false | false | true | true | false |
| unavailable（再試行可） | false | false | true | false | true |
| unavailable（再試行不可） | false | false | true | false | false |
| closed | false | false | false | false | false |

requesterで取得した結果もstate=readyなら、押した本人だけに見えるメッセージに内容と許可選択を出す。取得不能をrequesterで回避しない。readyはその表示先に必要な情報が揃う意味で、安全性の保証ではない。

unavailable／closedはpresentation_id、presentation_fingerprint、expires_at、pageをnullとし、表示版の上限を消費しない。private_requiredは元会話向けの説明版を発行できるが、許可には使えない。callを確認できない場合はturn_id／scope／scope_fingerprintとtool_policyをnull。scopeの新世代を確定できない場合もscope／scope_fingerprint／tool_policyはnullとする。監査用に過去のscopeが残る場合の扱いは第11節による。displayは必ずあり、内容未取得のprovenanceはunavailableとする。

## 4. 診断と再照会

| reason／diagnostic.code | state | 再試行 | 人間向けの意味 |
| --- | --- | --- | --- |
| catalog_loading | unavailable | 可 | ツールの定義を取得中です |
| catalog_timeout | unavailable | 可 | 定義の取得に時間がかかり、確認できませんでした |
| catalog_rpc_failed | unavailable | 可 | 定義を取得できませんでした |
| catalog_server_unavailable | unavailable | 可 | この操作に必要なサーバーが利用できません |
| catalog_auth_required | unavailable | 不可 | 接続先への認証が必要です |
| catalog_too_large / catalog_capacity | unavailable | 不可 | 定義量／管理件数が上限を超えました |
| catalog_invalid | unavailable | 不可 | 定義の応答を検証できませんでした |
| catalog_generation_changed | unavailable | 可 | 設定の変更を確認中です |
| definition_mismatch | unavailable | 不可 | 登録した定義と現在の定義が異なります |
| unsupported_renderer | unavailable | 不可 | この操作の表示がまだ実装されていません |
| operation_semantics_unverified | unavailable | 不可 | 実行する内容を確定できませんでした |
| personal_information / private_communication | private_required | 不可 | 個人情報／個人宛てのやり取りを、押した本人だけに表示します |
| authentication_information / executable_code | private_required | 不可 | 認証情報／実行コードを、押した本人だけに表示します |
| privacy_unclassified | private_required | 不可 | 他の人に見せてよい内容か判断できないため、押した本人だけに表示します |
| display_too_large | private_required（元会話用） | 不可 | 一度に表示できないため、押した本人だけに内容を分けて表示します |
| information_incomplete / unknown_arguments | unavailable | 不可 | 判断に必要な情報が足りない／引数を検証できない |
| operation_unavailable | unavailable | 不可 | 操作情報を取得できません |
| interaction_closed | closed | 不可 | 確認は終了しています |
| presentation_limit | unavailable | 不可 | 表示の更新回数が上限を超えました |

private_requiredのreasonはrequester表示で内容が揃えばnullに変わる。コードを本人向けに表示しても、コード実行が依頼中許可へ昇格することはない。個人情報等を除去して「内容は揃った」と偽らない。判定できない情報は未知のまま返す。

再試行可ではretry_after_ms=2000、それ以外null。同じinteractionのGETを再照会する。実行要求／承認返信は再送しない。Gatewayは重複した取得中カードを連投せず同じ通知を更新する。Proxyのcatalog取得はHTTP GETから切り離し、GETは最大250msで現在状態を返す。詳細はカタログ設計を参照。

## 5. 本文・上限・本人向け表示

公開用displayは既存title/fields/limitations/omissionsとprovenance/disclosureを維持する。最大4800 UTF-16 units、fieldsは24、JSON全体は64KiB。全ラベル・値・制限文を数える。各field.valueは最大1000 units、titleとlabelは各200 units。値を切り詰めたり意味を省略して上限へ合わせない。改行・タブは意味を保持する可視表現で示し、変換した場合はlimitationsへ記載する。メンションや装飾を実行しない。

GatewayはDiscordへ送る最終文字列でさらに上限を検査する。エスケープで増えた文字、ボタンや自分が追加した説明も含める。収まらなければ公開での許可ボタンは出さず、本人向けの完全な表示へ移る。display_too_largeとプライバシーを混同しない。

requester表示は最大64ページに分けて取得できる。HTTP応答は各64KiB、各ページ4800 UTF-16 units／24 fields以内、全ページのdisplay JSON合計256KiB以下。上限を超える場合はinformation_incompleteで許可不可。分割は省略とは違い、全文を順番に確認できる。元会話向けは常に1ページとし、収まらない内容を一部だけ公開して許可させない。

`GET /interactions/{id}/presentation?audience=requester&page=N&presentation_id=...` で後続ページを取得する。初回はpage=0（省略可）、presentation_idは初回応答のID。pageは0以上count未満。page>0でID欠落、不正型、不正な範囲は400 invalid_presentation_page。版が変わった場合は409 presentation_stale。page=0を含め同じIDを再指定できる。

長い単一の値も切り捨てず、Unicode scalarの途中を切らない連続した断片として分ける。同じ項目の続きであることと断片の順序を固定ラベルに明記し、このラベルも表示上限へ計上する。配列・構造化条件は要素や式の親子関係を繰り返し示し、分割でAND/OR・否定・変更対象の対応を曖昧にしない。安全に分割できない場合はinformation_incompleteとし、先頭だけで許可しない。

全応答にpageを追加する。ready／private_requiredではindex/count/token/content_fingerprintを持つobject、unavailable／closedではnull。tokenは当該ページを識別する不透明値（256 bits以上、最大256 ASCII bytes）、content_fingerprintは全ページの内容をプロセス専用鍵で拘束する不透明値。生本文のSHA等、推測した秘密値を照合できる無鍵のdigestを公開しない。元会話と本人向けで共用しない。ページごとに版を増やさず、全ページが同一のpresentation_id、presentation_fingerprint、scope、revision、audience、期限を持つ。

Gatewayは本人向けページを順序付きで表示し、何ページ中の何ページかを明示する。ページ切替自体を許可と解釈しない。actionsは表示全体に対するProxyの適格性であり、個々のページへの許可ではない。全ページの表示が成功したことを保存するまで許可ボタンを出さず、最後に許可を選べる。拒否は途中のページでも選べ、全ページの取得・表示を要求しない。Proxyは画面を実際に読んだことまでは検証できないが、返信時に全ページに対応するtokenが揃うことを検証する。表示成功の確認はGatewayの責務。未確定配信のページを確認済みにしない。

ページの本文をGatewayのDBへ保存しない。保存するのはページ番号／token／content_fingerprint／本人との対応／配信したmessage IDのみ。再起動後に必要なページを取得できないなら許可せず再表示を案内する。取得中に版が変わったら確認済みページを全て破棄する。

omissionsが1件でもある表示に許可ボタンを出さない。認証の秘密値をUIへ再露出する必要がない専用認証フォームは本APIへ一般化せず、既存mcp_formの認証手順を使う。

## 6. 依頼中の許可と設定

scopeは従来のinstance/recovery/principal/channel/run/Response/conversation/workspace/Turn/input generation/config generation/server/toolに、policy_generation／definition_generationを追加する。許可対象は「このpolicyで評価された当該ツールの通常操作」であり、別の引数を無条件に全て許可する意味ではない。

新policyは初回作成と各後続呼出しの両方へ適用する。外部送信・共有権限変更・削除・任意実行・秘密入力・認証が最優先の個別確認条件。複合toolにread grantがあっても、deleteやsendへ適用しない。tool_policy.effectsはread/write/delete/external_send/permission_change/execute/credential/session_change/unknownの列挙。eligible_operations／always_confirm_operationsは登録表の固定ID配列（各最大64、ID128 bytes以内）であり、引数値・接続秘密を含めない。

公開カードと本人向けカードで適格性を同じように評価する。tool_policy.reasonはnullまたはbinding_unavailable/catalog_unavailable/policy_unverified/operator_restricted/always_confirm_effect/privacy_unverified/config_changed/input_changed/scope_ended。原因を操作名や推測で補わない。個別toolの評価が未完了ならpolicy_unverifiedを返し、正常な完成例として試験を合格にしない。

運用設定の改訂案：mcp_turn_grant_toolsが省略されていれば評価済み通常操作の登録表を使う。明示した空mapは全て個別確認、明示したmapはその部分集合へ制限する。名前を追加しても上位の個別確認条件は解除されない。これは表示の公開許可設定ではない。

この省略時の既定は0.5 Responseだけに適用する。0.3/0.4は従来のallowlist評価を維持し、省略時は空のallowlist（ターン許可なし）とする。詳細は第11節の互換表による。

本番のbrowser_findだけの明示設定は、全体受入後の配備時に省略形へ移行し、評価済み通常操作を対象化する差分として提示する。実装前に変更しない。既存利用者の制限設定を更新時に勝手に削除しない。追加のON/OFF項目は作らない。

## 7. 返信・失効・競合・監査

0.5のready表示から許可する場合は、従来bodyに以下を必須とする。

```json
{
  "expected_revision":1,
  "expected_scope_fingerprint":"opaque-scope",
  "approval_view":"source_conversation",
  "expected_presentation_fingerprint":"opaque-presentation",
  "expected_page_tokens":["opaque-page"],
  "grant_scope":"turn_tool",
  "response":{"action":"accept","content":{}}
}
```

approval_viewはsource_conversationまたはrequester。expected_page_tokensはページ順の全token配列（1〜64件）で、重複・不足・別版・別audience混入は409 presentation_incomplete。1ページの公開表示でも必須。単発はgrant_scopeを省略。本人向け表示のトークンと元会話向けトークンを交換して使えない。未知・片方欠落は400 invalid_approval_presentation。0.5の確認対象callに旧bodyで許可する迂回は拒否する。拒否は従来のrevision付きbodyを使え、非公開内容の閲覧を要求しない。0.3/0.4のResponseは従来契約を維持する。

同一request keyの既登録operationは先に照会する。未登録の許可では送信意思を保存するtransaction内でscope、現在のcall、世代、表示内容、privacy、actions、停止、TTL、catalogの有効性を再検証する。DB保存失敗時は上流へ許可を送らない。通信結果不明は再送しない。

| 場面 | 動作 |
| --- | --- |
| 表示後に内容／scope／世代が変わった | 409 presentation_stale、送信しない |
| public/requesterの取り違え | 409 presentation_audience_mismatch |
| catalogが期限切れ／更新中 | 409 approval_catalog_unavailable。再GETで現在状態を照会 |
| 期限切れ | 409 presentation_expired |
| 個別確認対象でturn_toolを指定 | 422 turn_grant_ineligible |
| Stop／cancel／Steer／終了と許可が競合 | 既存の送信意思境界で直列化。失効が先なら許可しない |
| 送信意思が先に確定した後の停止 | 送信済み許可を取り消したとは言わず、対象Turnを停止する |
| 復元・再起動 | 旧snapshot・表示・grantは再有効化しない |
| catalog取得失敗 | 過去の成功値で許可しない。失敗前の版を復帰で復活させない |

有効表示はcall／interaction期限以内で最大10分、GETで延長しない。各audienceに最大4版。同内容GETや障害待機表示で版を増やさない。DBに本文・生引数・秘密の公開ハッシュを保存しない。プロセス専用鍵で内容を拘束し、監査には操作ID・scope・不透明世代・固定の理由コードだけを残す。保持期間は既存v2の状態廃止まで、許可TTLと別。

## 8. Gatewayの表示・保存・再起動

1. Proxyのdisplayだけをそのaudienceへ表示する。操作名やモデル説明を承認の判断材料の代わりにしない。
2. 本人向けボタンでは本人・元会話・Response・版を先に照合し、Discordの本人だけに見える返信を使う。本人以外へ内容を返さない。
3. 実際に配信したmessage ID、audience、表示トークン、Proxyが返した不透明なcontent_fingerprint、scope、revisionを保存する。配信結果不明のカードから許可しない。原文や実引数をDBへ保存しない。
4. 押下時は最新表示を再取得し、配信版と一致して初めて返信する。異なる内容を古いボタンの許可として扱わない。
5. 再起動時は保存メタデータとProxyのoperation／interactionを照会する。秘密本文をディスクから再表示しない。本人向けの再表示は新しいボタン操作から取得する。再表示はAI再実行でも許可でもない。
6. 取得失敗は再確認を案内する。未対応を本人情報保護の理由へ偽装しない。読取の再試行と承認送信の再試行を区別する。

## 9. 受入と指摘対応

P-01はThread別の有界catalog取得、P-02は診断と非同期取得、P-03は全件台帳と共通の表示形式、P-04は引数ごとのpolicyと設定移行、G-01はGatewayの完全表示要件、T-01は実構成と実Discordを別に評価する受入で対応する。

F01〜F14は[検証記録](mcp-approval-full-fix-validation.ja.md)。342ツール全件の定義を使った正常／異常fixture、実構成342件の取得、通常操作の実行と初回カード、本人向け表示の別人拒否、停止／競合／復元／送信不明を含む。ツール定義を確認しただけで342件の実動作を確認済みとしない。無差別な外部送信・削除を試験しない。

## 10. 実装開始前に残す具体作業

本書のAPI形式をGatewayが先に照合できるよう提示するが、以下はProxyの詳細設計として引き続き完成させる。ここを実装しながら決めない。

- 全342件の操作名と1,082引数の表示ラベルは付与済み。実定義との意味照合、複合操作の条件式、肯定／否定fixtureの対応を完成させ、台帳の未評価印をなくす。
- 台帳の構造化検討4引数・本文等の検討20引数の意味評価を完了する。規則を付けた構造化30引数・本文等42引数についても、ラベル・fixture・実接続版の照合を完了する。内包操作のschemaが不足する箇所は実資料で調べ、未知と意図した例外を区別する。
- [公開範囲の判定仕様](mcp-approval-privacy-design.ja.md)の具体例を各ツールのフィールド・内包操作へ対応させる。文法の範囲外と検出限界を明示し、未分類を全件公開／全件本人限定へ倒さない。
- 第5節の分割表示と全ページtokenの接続レビューは完了。Gatewayは表示・DB・再起動復旧の内部詳細設計を担当し、双方の実装開始条件を区別して確認する。省略内容での承認をしない。

Gateway接続指摘は解消済み。残る上記内部詳細設計の完了と関連文書の整合を満たしてから製品実装へ進む。常駐配備・コミット・push・実Discord受入は別に記録する。

現版のNotion update_contentは利用者決定により実行不可とする。入力不正はunknown_arguments、検証済み入力でも実行時の本文版を保証できない場合はoperation_semantics_unverified。いずれもunavailableで許可操作なし。本人向けへ移すだけで許可可能にしない。既存enumと表示契約内の扱いであり、JSON項目は追加しない。詳細は[Notion設計](mcp-approval-notion-design.ja.md)。処理・保存・競合は[Proxy内部設計](mcp-approval-v05-proxy-internals.ja.md)を正本とする。

## 11. GatewayレビューR-01：照会API・保存・適格性

本節は0.5 Responseだけに適用する。0.3/0.4のJSONへ新必須フィールドを混入しない。標準OpenAI互換APIに変更はない。

### 11.1 正本と新世代

- `GET /v2/codex/interactions/{id}/operation` は本人向けの操作詳細。0.5では必須の`profile`、`tool_policy`を追加し、scopeへpolicy_generation/definition_generationを追加する。
- `GET /v2/codex/interactions/{id}` のoperationは受理時の監査メタデータ。同じ新フィールドを保存するが、現在のボタン判定には使わない。interaction一覧に同じoperationを含める場合も同じ形式。
- presentationへ必須の`scope`を追加する。同一call・revision・scope_fingerprint・評価世代のoperationと同じscope。新世代は不透明な非空stringで既存のID上限8192 bytesに従う。
- policy_generationは評価規則の版、definition_generationは対象Thread/server/toolの確認済み定義と無効化世代を表す。正常な同一定義のTTL更新では変えない。失敗後の復帰・定義変更・再構築では旧値を再使用しない。他serverの個別障害だけで正常serverの世代を変えない。
- 新情報の正本はProxyのこれらの応答。Gatewayは旧canonical_scopeの項目だけを抜き出さず、Responseのprofileごとの全scopeフィールドを検証・保存・比較する。未知の必須形式を黙って落とさない。

0.5の操作詳細の完全例（pending・通常のタブ一覧）：

```json
{
  "interaction_id": "int_123",
  "response_id": "resp_123",
  "turn_id": "turn_123",
  "call_id": "call_123",
  "server": "playwright",
  "tool": "browser_tabs",
  "binding_status": "verified",
  "unavailable_reason": null,
  "config_generation": "config-opaque",
  "input_generation": 0,
  "scope": {
    "instance_id": "pxy_example",
    "recovery_generation": "recovery_example",
    "context": {
      "principal_id": "opaque-user",
      "channel_id": "opaque-thread",
      "run_id": "run-123"
    },
    "response_id": "resp_123",
    "conversation_id": "conv_123",
    "workspace_id": "ws_123",
    "turn_id": "turn_123",
    "input_generation": 0,
    "config_generation": "config-opaque",
    "server": "playwright",
    "tool": "browser_tabs",
    "policy_generation": "policy-opaque",
    "definition_generation": "definition-opaque"
  },
  "scope_fingerprint": "opaque-scope",
  "revision": 1,
  "turn_grant_eligible": true,
  "ineligible_reason": null,
  "arguments": {
    "action": "list"
  },
  "redacted_paths": [],
  "disclosure": "requester_only",
  "profile": "source-conversation-v2",
  "tool_policy": {
    "policy_id": "evaluated-normal-operations-v1",
    "policy_generation": "policy-opaque",
    "definition_generation": "definition-opaque",
    "effects": [
      "read"
    ],
    "turn_eligible": true,
    "reason": null,
    "grant_scope": "turn_tool",
    "eligible_operations": [
      "list",
      "new",
      "select"
    ],
    "always_confirm_operations": [
      "close"
    ]
  }
}
```
0.5の追加フィールドは必須であり、未取得を省略で表現しない。対応付け不能なら旧契約どおりcall/server/tool/config/input/argumentsをnullとし、scope/scope_fingerprint/tool_policyもnull、turn_grant_eligible=false、ineligible_reason=binding_unavailable。未取得／更新中／失敗で新世代を確定できないときは、確認済みのcall/server/tool/config/inputを保持できるが、scope/scope_fingerprint/tool_policyはnull、turn_grant_eligible=false、ineligible_reason=catalog_unavailable。argumentsを保持していることは許可可能の意味ではない。

終端・期限切れ・再起動後は監査用scopeと世代を保持できるが、arguments=null、unavailable_reason=operation_details_expired、turn_grant_eligible=falseとする。現在のpolicy再評価ができない場合tool_policy=null。監査メタデータの過去の適格性を現在の許可可能性へ読み替えない。無効な表示のscope_fingerprintは許可に使えない。

取得不能理由は0.3の列挙を維持し、カタログの詳細理由はpresentation.diagnosticで返す。0.5のineligible_reasonはtool_policy.reasonの列挙を使う（旧high_risk_tool等へ逆変換しない）。scopeとtool_policyに重複する世代は必ず一致させる。

### 11.2 ボタン判定

**0.5のボタン判定の正本は最新presentationのactionsとtool_policy。** operation.turn_grant_eligibleは同じ評価器の結果を返し、同じrevision・scope_fingerprint・世代・評価時点ではtool_policy.turn_eligibleと一致する。ただしprivate_requiredの公開presentationではturn_eligible=trueでもactions.allow_turn_tool=falseになり得る。全ページの表示成功・本人認可を満たした後、state=ready、actions.allow_turn_tool=true、tool_policy.turn_eligible=trueの全条件でだけ依頼中許可を出す。旧operationのbooleanだけでボタンを出さない。

別々のGETの間にTTL・世代・停止が変化し得る。scope/revision/fingerprintが違えば新しい表示からやり直し、異なるスナップショットの許可条件を合成しない。同じ版でも最新presentationがloading／失効を返したら古いtrueを使わない。interaction.operation内の受理時booleanはこの比較対象ではない。

### 11.3 interactionと冪等operation

単発／依頼中許可の返信を保存した後のGET interaction完全例（上流解決前）：

```json
{
  "interaction_id": "int_123",
  "response_id": "resp_123",
  "conversation_id": "conv_123",
  "workspace_id": "ws_123",
  "kind": "mcp_form",
  "state": "submitted",
  "revision": 3,
  "created_at": "2026-09-17T12:00:00Z",
  "expires_at": "2026-09-17T12:10:00Z",
  "request": null,
  "reply_status": "written",
  "resolution_reason": "interaction_reply_written",
  "error": null,
  "grant_id": "grant_123",
  "operation": {
    "binding_status": "verified",
    "call_id": "call_123",
    "server": "playwright",
    "tool": "browser_tabs",
    "config_generation": "config-opaque",
    "turn_grant_eligible": true,
    "ineligible_reason": null,
    "disclosure": "requester_only",
    "redacted_paths": [],
    "scope": {
      "instance_id": "pxy_example",
      "recovery_generation": "recovery_example",
      "context": {
        "principal_id": "opaque-user",
        "channel_id": "opaque-thread",
        "run_id": "run-123"
      },
      "response_id": "resp_123",
      "conversation_id": "conv_123",
      "workspace_id": "ws_123",
      "turn_id": "turn_123",
      "input_generation": 0,
      "config_generation": "config-opaque",
      "server": "playwright",
      "tool": "browser_tabs",
      "policy_generation": "policy-opaque",
      "definition_generation": "definition-opaque"
    },
    "scope_fingerprint": "opaque-scope",
    "profile": "source-conversation-v2",
    "tool_policy": {
      "policy_id": "evaluated-normal-operations-v1",
      "policy_generation": "policy-opaque",
      "definition_generation": "definition-opaque",
      "effects": [
        "read"
      ],
      "turn_eligible": true,
      "reason": null,
      "grant_scope": "turn_tool",
      "eligible_operations": [
        "list",
        "new",
        "select"
      ],
      "always_confirm_operations": [
        "close"
      ]
    }
  }
}
```
単発ならgrant_idは従来どおり省略。自動適用した後続interactionにもgrant_idを保存する。operationの新情報は受理時のスナップショットであり、後で許可が取り消されても履歴を上書きしない。call未対応付けならoperation自体は従来どおりnullを許容する。

`GET /v2/codex/operations/{operation_id}` と `/operations/by-key/{key}` の**冪等書込みoperationは変更しない**。上の「MCP操作詳細」と同名だが別の資源である。返信のresource.type=interactionからGET interactionへ進み、grant_idがあれば許可一覧を照会する。取消も従来どおりresource.type=mcp_grant。これらに現在の適格性を追加しない。

### 11.4 許可一覧の完全例

`GET /v2/codex/responses/{response_id}/mcp-grants`。0.5では各grantへprofile、grant_policy、availabilityを必須追加する。grant_policyは作成時に利用者が許可した不変の条件、availabilityは照会時に適用を試みられるかを表す。grant.state=activeだけで「今すぐ自動適用可能」と判断しない。

```json
{
  "response_id": "resp_123",
  "data": [
    {
      "grant_id": "grant_123",
      "scope": {
        "instance_id": "pxy_example",
        "recovery_generation": "recovery_example",
        "context": {
          "principal_id": "opaque-user",
          "channel_id": "opaque-thread",
          "run_id": "run-123"
        },
        "response_id": "resp_123",
        "conversation_id": "conv_123",
        "workspace_id": "ws_123",
        "turn_id": "turn_123",
        "input_generation": 0,
        "config_generation": "config-opaque",
        "server": "playwright",
        "tool": "browser_tabs",
        "policy_generation": "policy-opaque",
        "definition_generation": "definition-opaque"
      },
      "state": "active",
      "reason": null,
      "created_at": "2026-09-17T12:00:00Z",
      "expires_at": "2026-09-17T12:10:00Z",
      "application_count": 1,
      "initial_interaction_id": "int_123",
      "profile": "source-conversation-v2",
      "grant_policy": {
        "policy_id": "evaluated-normal-operations-v1",
        "policy_generation": "policy-opaque",
        "definition_generation": "definition-opaque",
        "grant_scope": "turn_tool",
        "allowed_effects": [
          "read",
          "session_change"
        ],
        "always_confirm_effects": [
          "delete",
          "external_send",
          "permission_change",
          "execute",
          "credential",
          "unknown"
        ],
        "eligible_operations": [
          "list",
          "new",
          "select"
        ],
        "always_confirm_operations": [
          "close"
        ]
      },
      "availability": {
        "state": "ready",
        "reason": null,
        "retry_after_ms": null
      }
    }
  ]
}
```
grant_policyは全項目必須・null不可。scopeと両世代は一致必須。操作ID配列は第6節の上限、作用配列は同節のenumから重複なく列挙する。allowed_effectsは当該toolの評価済み通常作用だけ。条件の未知・内包操作の未評価は適用しない。秘密入力・認証はcredentialへ分類し、公開範囲の規則も毎回再適用する。状態がexpired/revokedでも過去の条件を保持する。

availability.stateはready/refreshing/inactive。readyはreason=null、retry_after_ms=null。refreshingはreason=catalog_loading、retry_after_ms=2000。inactiveはreasonを必須とし、activation_pending/catalog_failed/catalog_changed/policy_changed/config_changed/input_changed/scope_ended/operator_revoked/expired/runtime_restarted/response_unknownのいずれか、retry_after_ms=null。readyは次のcallの自動許可を約束せず、対象・全引数と有効期限を適用時に再検証する。

pendingはavailability=inactive/reason=activation_pending、suspendedはinactive/response_unknownとする。通常のTTL更新はgrant.state=active、availability=refreshing。suspendedは従来の承認返信結果不明専用であり、更新待ちに流用しない。取得失敗・定義変更はgrantをrevokedにしreason=catalog_failed/catalog_changed、ポリシー変更はpolicy_changed。期限切れはexpired、取消はrevoked/operator_revoked。再起動はrevoked/runtime_restarted。既にrevoked/expiredなら後発理由で元の終端理由を上書きしない。世代・停止等の既存失効も同様に適用する。

一覧は従来の最大16件・1MiB・ページングなしを維持。旧Responseは0.3の形と条件のままであり、新規項目は省略。Gatewayが複数Responseの一覧をまとめる場合はResponseごとにprofileを確定して別々に検証する。新Responseへ旧grantを移植しない。保存した一覧からボタンや自動承認を復元せず、Proxyの現在の一覧とoperationを照合する。

### 11.5 capabilityと設定の組合せ

0.5のpresentation.enabled=trueには既存mcp_operation_details.enabled=trueが必要。新profileの選択には両方と既知profileの検証が必要。既存mcp_turn_approval.enabledは依頼中許可の追加条件であり、falseなら単発表示を提供できる構成でも依頼中許可はfalse。現行の共通運用スイッチがOFFなら関連機能もOFFで、依存が矛盾したcapabilityはGatewayが使用しない。旧mcp_inline_approvalのenabledは0.4用であり0.5を代替しない。

| Response | mcp_turn_grant_tools省略 | 明示空map | 明示browser_findのみ |
| --- | --- | --- | --- |
| 0.3/0.4 | 従来の空allowlist。依頼中許可なし | 依頼中許可なし | 従来の適格性と制限内でfindのみ |
| 0.5 | 評価済み通常操作policy | 依頼中許可なし | 新policyの条件を満たすfindだけ |

どの場合も上流が常時許可する操作へ追加の承認を作らない。設定を変えればconfig_generationを更新して既存grantを失効させる。設定省略を旧Responseの許可拡大に使わない。0.5を使う新Gatewayでも過去Responseのprofileを黙って変更しない。

## 12. GatewayレビューR-02：定期更新と後続呼出し

通常のTTL切れは、許可を取り消す出来事ではなく**最新の定義が確認できるまで適用を待つ状態**とする。古いcatalogのまま自動許可しないが、同一世代・同一定義の再取得成功後はProxyが既存grantを再評価する。Gatewayが許可を代行送信する方式にはしない。

- 有効grantに対応し得る後続callを受け、catalogだけがstaleなら、同一interactionをpendingのまま内部のrefresh_waitへ置く。新しい承認カードを連投しない。GET presentationはcatalog_loading、許可ボタンなし、拒否は可能。Gatewayは必要なら同じ通知を「接続先の情報を更新しています」に更新する。
- 待機期限はcall受付から60秒、共有catalog workerの期限、interaction期限、grant期限の最も早い時刻。単調時計で計測し、GET・ページ切替・worker再作成で延長しない。停止／Steer／取消は期限まで待たず即時に再評価を止める。上限16件の未回答interaction枠を共用し、別の無制限待機キューを作らない。
- 正常に同一定義へ更新したら、元grantがactiveか、全scopeと世代が同じか、期限内か、同じcallの実引数と作用・公開範囲がpolicyを満たすかを再評価する。全条件一致のときだけProxyが一度だけ送信意思を保存し上流へ適用する。表示版とgrantの元期限を延長しない。
- grant期限切れ・取消・不適格な後続引数なら自動適用しない。interaction自体がまだpendingなら更新後の最新表示で個別に確認する。失敗や停止と違い「利用者が拒否した」とは記録しない。
- catalog取得失敗は旧grantを失効させる。復帰して同一定義でも復活させない。定義・policy・設定・入力世代が変わった場合も旧grantを使わない。停止・Run終了等でinteractionが終わればカードも閉じる。
- catalog workerの60秒取得期限に間に合わなければcatalog_timeoutとしてその取得を失敗確定し、旧grantを失効させる。call受付から60秒の待機期限が先なら当該callの自動適用待機を終了し、そのcall用の旧grantを失効させるが、他callと共有するworkerの失敗を捏造しない。grant期限切れだけをcatalog_timeoutに置き換えない。interactionの期限が先に来たら既存の期限終了処理を行い、成功としない。期限後の遅着結果を、そのcallの自動許可へ結び付けない。
- 更新成功時の自動適用、利用者の拒否・手動許可、Stop／Steer／revokeを同じ送信意思境界で直列化する。勝者だけが操作を確定し、他は既存operation／実状態を返す。更新中に古い許可ボタンが押された場合は409 approval_catalog_unavailableで送信しない。同じcallへ後から二度許可を送らない。

通知は既存interaction変更通知とGET復旧を使う。refresh_waitを新しいinteraction.state列挙へ追加しない。Gateway再起動はProxyの待機やgrantを消さないが、Proxy再起動は旧許可を失効させる。クライアント切断も共有workerを止める理由にしない。

## 13. 今回のレビュー対応と未完了範囲

R-01は第11節と第3節のscope、R-02は第12節とカタログ設計に対応を記載した。Gatewayによる第11〜13節の再照合でR-01/R-02は解消し、接続仕様は合意済み。長文のDiscord送信・全断片の配信確定・保存・再起動復旧はGatewayが担当し、Proxyのページtoken検証だけで完了としない。第10節の全ツール詳細設計ゲートも維持する。**この追記は実装Go・コード変更・常駐反映ではない。**

責務整理合意後の接続先は[API 0.6レビュー案](mcp-approval-api-v06.ja.md)。本書の技術的知見は引き継ぐが、単発承認への意味評価必須条件・全実行への禁止波及・全342件の着手ゲートは後継要件に従って置き換える。
