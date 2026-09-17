# 汎用MCP承認・ポリシー拡張 API 0.6 — 接続合意版

2026-09-17。責務整理は利用者・Gateway双方合意済み。本書はC06-01・02を反映した接続合意版。利用者からGatewayの再照合完了・接続Goと実装開始指示を受領した。製品実装とProxy単独の受入試験は完了。実Codex・実Apps・Gateway型との照合結果と常駐反映は[実装記録](mcp-approval-v06-implementation.ja.md)を参照。Discordを含む統合試験は利用者の指定によりGateway側で実施する。0.5のwireを無告知で変更せず、新profileで区別する。

要求・応答の完全な追加オブジェクトと承認資源の例は[JSON例](mcp-approval/api-v06-examples.json)。既存v2エンドポイント・認証・冪等書込み・会話／ワーク・状態保持契約は維持する。本書はMCP操作の承認を対象とし、任意のMCPフォーム入力を空contentのacceptへ変換しない。

## 1. 責務と変更の要点

- 中核：Codexが実際に要求した承認を、実call・全引数・利用者・実行へ対応付けて仲介する。全ツールの意味理解は前提にしない。
- 表示：実callと引数の忠実な表示を保証する。評価済み説明は追加できる。表示指定だけでは追加ポリシーを選ばない。
- ポリシー：明示選択された実行だけへ、評価済み依頼中許可や禁止規則を適用する。上流設定の制約もその範囲に閉じる。
- クライアント：本人認可、表示、ページ全体の配信、具体UI、配信復旧を担当する。Discord専用フィールドは増やさない。

意味未評価は、単発承認不可と同義ではない。安全性の評価結果、原引数の完全性、公開可否、ポリシーによる実行禁止を別フィールドで表現する。

## 2. capabilityと明示選択

`GET /v2/codex/capabilities`へ独立キー`mcp_approval_v06`を追加する。旧キーの形・値を置換しない。必須フィールドはJSON例のcapabilityに示す。

profile=`source-conversation-v3`、rendererは`raw-arguments-v1`と`evaluated-operation-v1`。どちらも同じページ・fields形式を使う。中核の単発表示enabledと、各policyのenabledは別。既存mcp_turn_approval_enabled=falseでも新しい汎用単発表示を無効にしない。未実装ならキーを出さず、実装済みだが利用不可ならenabled=falseとする。

初期登録policy（識別子とversionの組は不変）：

| id | version | 内容 |
| --- | --- | --- |
| evaluated-turn | 1 | 評価済みの通常操作だけへ依頼中許可を提供。上流が元から要求する承認の範囲で作用する |
| evaluated-turn-notion-guard | 1 | 上記に加え、保証できないNotion部分置換を禁止。その検査に必要なNotion更新ツールの承認を上流に強制する |

`policies[]`はid/version/enabled/label/restrictions/upstream_overridesを必須とする。labelは表示用固定文、restrictionsは固定規則ID配列、upstream_overridesは固定説明文配列。本文や接続秘密を入れない。未知policyを似た名前のpolicyへ置換しない。Gateway向けの推奨選択はevaluated-turn-notion-guard。別のクライアントも同じ契約で選べる。

`POST /v2/codex/responses`の追加項目：

- `approval_presentation={"mode":"source_conversation","profile":"source-conversation-v3"}`。
- `approval_policy=null` または `{"id":"evaluated-turn-notion-guard","version":1}`。省略はnullと同じ。未知キー・型違いは400。
- `approval_context`と`interaction_capabilities:["mcp_form"]`は従来の本人・会話・実行境界。0.6表示には有効なcontextが必須。

policyは表示から独立した項目だが、初期0.6の拡張を指定するには0.6表示profileを必要とする。policyだけを旧クライアントの返信形式で指定した場合は400 approval_policy_requires_v06。これは無告知の機能有効化を避ける接続条件であり、表示を指定すればpolicyも付くという意味ではない。

追加選択は同じRunのSteerで変更不可。次Responseでは再指定が必要で、省略に前Runのpolicyを継承しない。既存の同一request keyには、保存した元要求の結果を返す。違うselectionで同じキーを使えば既存の冪等競合。policyの選択／不選択は要求fingerprintに含める。省略とnullは同じ未選択として正規化する。

## 3. 受付と実効ポリシーの照会

202は永続受付であり、上流設定の準備完了ではない。新しいbinding IDと受付時に選択したpolicy版を保存してから返す。実行開始前に設定の適用と条件を検証する。準備不能ならAI Turnを開始せず既存v2の実行失敗処理へ進む。準備結果不明を成功扱いして進めない。

0.6 Responseの受付応答と`GET /v2/codex/responses/{id}`へ`approval_policy`を必須追加する。202等の既存外枠にJSON例response_policy_pendingのオブジェクトを追加する。GETも同じ型を使用する。

| フィールド | 型と意味 |
| --- | --- |
| selection | nullまたは要求のid/version。実行の途中で変更しない |
| binding_id | 全0.6実行で非空の不透明ID。未選択でも発行し、前実行の設定を使わないことの照合対象にする |
| generation | 選択policyの実効規則・設定を指す不透明string。準備前／未選択はnull |
| state | preparing / ready / failed / closed |
| reason | null、policy_setup_failed、policy_setup_unknown、policy_configuration_conflict、scope_ended |
| restrictions | 当該版の固定規則ID配列。未選択は[] |
| upstream_overrides | 当該版の固定説明文配列。未選択は[] |
| preparation | 第13節の開始時刻・期限・復旧・Turn送信境界・設定隔離の照会情報。全状態で必須 |

未選択でも、前Runの上流設定を解消する準備が必要ならpreparing。readyは設定準備が整った意味で、上流操作を承認した意味ではない。選択済みreadyにはgeneration必須。closedは過去のselection／binding／generationを保持し、次Runで有効とは扱わない。

状態遷移の詳細は第13節。基本はpreparing→ready→closed、preparing→failed。準備期限切れはfailed、未開始の停止はclosed。再起動時の限定的なready→preparingと占有状態は第13節に従う。送信済み操作の成否は既存interaction／operationの状態で照会し、policy.stateで代用しない。実行中の不整合を検出したら新たな許可適用を止め、既存の結果不明・停止処理に移る。単にselection=nullへ落として継続しない。

## 4. presentationとscope

既存GET `/v2/codex/interactions/{id}/presentation`と`?audience=requester&page=N&presentation_id=...`を使用する。0.6の必須トップレベルはJSON例presentation_unreviewedに全て示す。0.5からの主な差分：

- profileはsource-conversation-v3。
- `execution_policy`を必須追加。Responseのapproval_policyと同じ型・同じbinding。
- `argument_integrity`を必須追加：`{"status":"complete"|"unavailable","source":"codex_call_event","reason":null|string}`。
- `semantic_assessment`を必須追加：status=`evaluated|unreviewed|unavailable`、reason=`null|policy_not_selected|tool_not_evaluated|definition_changed|catalog_unavailable`、effects=`null`または0.5の作用enum配列。unreviewed/unavailableではeffects=null。空配列で安全性を表現しない。
- `tool_policy`はnullを許容する。未選択ならnull。選択時の型は第6節。
- `scope`へ`execution_policy_binding_id`を追加。`policy_generation`と`definition_generation`はnullableにする。それ以外は0.5と同じ必須フィールド。

scopeは実callのbindingが確定していれば、意味評価未完了でも作れる。policy_generationはexecution_policy.generationと一致。definition_generationは評価根拠の有効な定義があるときだけ非null。どちらも架空値を作らない。原引数fingerprintはcatalogがなくても、信頼された実callイベント・全引数・runtime設定世代・call IDを根拠にする。call自体が未確認ならscope／fingerprintをnullにして許可しない。argument_integrity.reasonはnullまたはcall_unbound/arguments_unavailable/arguments_invalid/arguments_too_large/operation_details_expired。status=completeではreason=null、unavailableでは理由必須とする。

0.6のscope fingerprintは全フィールド・値と確認対象を拘束する。クライアントはnull項目も落とさず保存し、文字列結合で独自に再計算しない。初回と許可時のcurrent stateを再検証する。

## 5. 汎用表示・公開範囲・ページ

raw-arguments-v1はCodexの実callイベントに含まれる全引数を型と構造を保って表示する。ユーザーやモデルの元の文字列を復元した保証ではない。JSON objectのキー、配列の順序、空集合、null、真偽値、数値、文字列を区別し、原引数に存在しない既定値を作らない。整数を浮動小数へ丸めない。重複キーや不正JSONなど、忠実な解釈ができない場合はargument_integrity=unavailable。上流の不明な引数キーを削らず表示する。

displayは0.5と同じtitle/fields/limitations/omissionsを持つ。固定の日本語ラベルに加え原キーと型を示す。入れ子はJSON Pointerと要素位置で結び付け、値全体を複数field／ページへ分ける。単純な平坦化で階層を失わない。フィールドが空でも「引数なし」と「引数を取得できない」を区別する。数値や文字列の表示上のescapeと原値の対応を明示する。

未評価表示には固定説明「Codexが求めた操作と入力値です。Proxyでは操作の意味・安全性を評価していません」を含める。評価済み説明を使っても、安全性の包括保証とは表示しない。動的なHTML、Markdown、メンション、リンクプレビューを実行せず表示する。

公開可否は単発承認の適格性と別。既存の評価済み公開ルールを使用できるものは元会話、秘密・個人情報は本人向け。未分類は本人向けでreason=privacy_unclassifiedとし、「個人情報を検出した」と偽らない。共有チャンネルへ生引数を無条件で公開しない。本人向けの完全表示では未評価を理由に許可ボタンを消さない。

0.5のページ上限を維持：応答65536 bytes、表示4800 UTF-16 units、fields24、本人向け64ページ、総表示262144 bytes、audienceあたり4版、元call期限以内で最大10分。原引数全体はUTF-8 JSONで262144 bytesまでを初期実装の対象とする。escape後の表示が上限を超えたらdisplay_too_largeの本人向け分割、それでも収まらなければinformation_incompleteで許可不可。省略を許可の代用にしない。

GET待機最大250ms、再試行目安2000ms、本文を永続化しないこと、全ページtoken、期限・失効、再起動復旧は0.5を継承。本人認可はクライアント責務とAPI認証を併用し、audience=requesterだけで本人確認済みとは扱わない。

## 6. 単発許可・ポリシー評価・禁止

選択policyのtool_policyは次の全フィールド必須：policy_id/version/policy_generation/definition_generation/effects/turn_eligible/reason/grant_scope/eligible_operations/always_confirm_operations/decision。

decisionは`not_blocked|blocked|unavailable`。not_blockedは追加禁止に該当しないという意味で、操作承認済みではない。effectsとdefinition_generationは評価不能ならnull。reasonは0.5の列挙にtool_not_evaluated/policy_denied/policy_check_unavailableを追加する。grant_scopeは適格時turn_tool、その他null。評価不能なら操作配列は[]。

| 状態 | 今回だけ許可 | 依頼中許可 |
| --- | --- | --- |
| 未選択、実call／全引数／完全表示を確認 | 可 | 不可 |
| 選択済み、通常評価済み、禁止なし | 可 | 全scope・世代・制限一致時に可 |
| 選択済み、未評価だが禁止規則の検査は完了 | 可 | 不可 |
| 選択policyの禁止に該当 | 不可 | 不可 |
| 禁止規則を検査できず回避の恐れがある | 不可 | 不可 |
| call／引数／完全表示が不足 | 不可 | 不可 |

Notion guardは正確な接続・tool identityで評価し、command=update_contentをblockedにする。禁止対象か検査できない入力はunavailableにし、汎用の単発経路で迂回しない。対象外ツールの意味未評価までこの理由へ一括分類しない。別の編集方法への自動変換は禁止。強制prompt等はguardを選んだ実行に限定する。

blockedはpresentation.state=unavailable、reason=policy_denied。表示には固定理由を出し、有効な許可tokenは発行しない。decision=unavailableはreason=policy_check_unavailable。拒否・停止は可能。policy未選択を表すtool_policy=nullでは、reasonの代わりにsemantic_assessment.reason=policy_not_selectedを使用する。

catalogの定期更新中は、既存grantに一致し得る後続callを0.5の有界待機で再評価する。自動許可のためのcatalog待機と、完全な原引数の単発表示を分離する。待機中に明示された手動単発許可は、禁止検査が可能なら確定できる。自動許可と競合したときは既存送信意思境界で一回だけ応答。意味評価やcatalogだけが不足しても単発許可を一律に止めない。

## 7. replyとエラー

既存`POST /v2/codex/interactions/{id}/reply`を使用。Idempotency-Key必須。0.5のexpected_revision/expected_scope_fingerprint/approval_view/expected_presentation_fingerprint/expected_page_tokens/responseを継承し、0.6では`expected_policy_binding_id`を許可時に必須追加する。依頼中許可を作る場合だけgrant_scope=turn_tool。単発は省略。未知・空文字のgrant_scopeを単発へ読み替えない。

拒否は従来どおりexpected_revisionとresponse.action=declineのみでよい。表示取得、ページ確認、policy binding照合を要求せず、認可・現在revision・確定済み返信との競合だけを検証する。JSON例を参照。

同一キーの既登録結果照会を先に行う。新規許可は同一transactionの送信意思保存前に、実call・全引数・scope・policy binding・禁止条件・表示・期限・停止を再照合する。単発に意味評価済みを要求しない。turn_toolだけは評価済みの完全なpolicy／定義世代が必要。送信不明をリトライで再送しない。

| HTTP／code | 条件 |
| --- | --- |
| 400 invalid_approval_policy | selectionの型・キー・version型不正 |
| 400 approval_policy_requires_v06 | 新policyを旧profile／表示宣言なしで指定 |
| 422 approval_policy_unknown | 未登録id/version。登録版への黙示変換なし |
| 503 approval_policy_unavailable | 登録済みだが新規受付不可。無効のまま受付しない |
| 409 approval_policy_binding_mismatch | expected binding不一致。上流送信なし |
| 409 approval_policy_not_ready | 当該実行の準備・設定検証が未完了 |
| 422 approval_policy_denied | 明示禁止に対する許可返信。上流送信なし |
| 409 approval_policy_check_unavailable | 適用中の禁止条件を検証不能 |
| 422 turn_grant_ineligible | 未選択／未評価／個別確認対象へのturn_tool |
| 409 presentation_stale / presentation_incomplete / presentation_expired / presentation_audience_mismatch | 既存の表示照合違反 |

0.6の単発にcatalog不足だけを理由とするapproval_catalog_unavailableを返さない。禁止検査がcatalogを必要とする場合はpolicy_check_unavailable、依頼中許可の評価待ちは既存catalog理由を使う。履歴の原引数が消失した場合は既存operation_details_expired。レスポンスエラーの外枠と冪等operationのstateは既存v2を維持する。

## 8. operation・interaction・grantの照会とDB

`GET /v2/codex/interactions/{id}/operation`の0.6型はJSON例operation_unreviewed。0.5の全既存フィールドを保持し、profile変更、新scope、execution_policy/argument_integrity/semantic_assessmentを追加する。tool_policyはnullable。未選択ならturn_grant_eligible=false、ineligible_reason=policy_not_selected。選択時はtool_policy.reasonに合わせる。

operationにはarguments_delivery=inline/presentation_pages/unavailableを必須追加する。応答全体が65536 bytes以内ならinlineでargumentsを返す。全引数は取得できているがinline応答に収まらない場合はpresentation_pages、arguments=null、unavailable_reason=nullとし、argument_integrity=completeを維持する。全文はpresentationの全ページから取得する。原引数自体が未取得・失効ならunavailable、arguments=null、unavailable_reasonに既存の実理由を設定する。nullだけを根拠に引数なし・取得失敗へ分類しない。

operationのargumentsは本人向けであり共有画面へコピーしない。単発許可に使う全文はpresentationの全ページを正本とする。argumentsがredacted／欠落／期限切れでも、元のinteractionへ拒否することはできる。監査にraw argumentsを保存しない。

`GET interaction`のoperationは承認返信の受理時メタデータ。0.6のprofile、scope、execution_policy、argument_integrity、semantic_assessment、tool_policy、grant_idを保存し、argumentsを含めない。これらを最新の許可可否へ読み替えない。現在のボタンは最新presentation.actionsだけを使う。

許可一覧・取消のURLは0.5と同じ。0.6のgrantにprofile、新scope、execution_policy（作成時）、grant_policy、availabilityを必須保存／返却する。grant_policyは0.5の全項目へpolicy_versionとexecution_policy_binding_idを追加。policy_idは選択id。scope.policy_generation／grant_policy.policy_generation／execution_policy.generationは同値、definition_generationは非null。未選択時は当該Responseのgrants=[]。過去Responseのgrantを返して埋め合わせない。

availabilityは現在の適用可能性、grant_policyは作成時の不変条件。状態・取消・in_flight_or_unknown_count・期限・再起動後失効は0.5を継承。冪等書込み`GET /v2/codex/operations/...`の外枠は変更しない。

クライアントDBはprofileごとに型を分け、新scopeのnullable値、binding、policy版、表示の評価状態を保持する。生引数や本人向け本文を保存しない。旧行を新profileに書き換えず、移行で不明な値を補完しない。

## 9. 同じ会話での次Run・上流設定の分離

同じconversation_idでpolicyを変更できる。変更は次Response受付時の明示指定だけで、同時進行／UNKNOWN中の占有は既存契約どおり禁止。前Runを確定するまで設定を切り替えない。

上流Threadの履歴を保持し、利用者にチャット再作成を要求しない。Proxyは前Runのguard設定を解除・再適用してから次Turnを開始する。変更後の条件を保証できなければ当該Responseの準備を失敗させ、古い設定で実行しない。新しい会話への黙示fork・AIの再実行は禁止。

Thread別のconfig overrideが実際に独立適用される経路、または互換性で分けたruntimeを使う。プロセス共通設定の変更で別実行へ作用させない。導入版のthread/start/resumeにconfigとapprovalsReviewerがあることはschemaで確認済みだが、既にロード済みThreadへの適用は別途実証する。これを接続契約の未定義ではなく、履歴維持・設定分離の内部受入課題として管理する。

標準API・旧profileには新policyを自動選択しない。0.6使用後の同じ会話を旧クライアントから続ける場合も、前Runの新policyを暗黙継承しない。既存の運用設定やCodexの常時許可を全クライアントで書き換えない。

## 10. 保持・復旧・互換・着手条件

Responseの選択・bindingと監査メタデータは既存v2 Response保持期限まで保持。生引数・表示本文は有界メモリのみ。秘密の無鍵ハッシュを永続化しない。grant／表示のTTLはGETで延長しない。Proxy再起動・復元でgrantとtokenを失効させ、旧generationで自動許可しない。Gatewayだけの再起動はProxy有効状態を照会し、ページ本文は再取得する。

0.3/0.4は現行互換、0.5は旧合意として保存、新実装・新Gatewayの接続目標は0.6。未配備0.5を実装済みとしてcapabilityへ出す必要はない。capabilityは実際に使えるprofileだけ提示し、未知profileへ自動downgradeしない。旧クライアントは新キーを無視して旧動作を使える。サーバー更新で既存Responseのprofileを変更しない。

汎用機能の着手条件は、本書の接続レビュー、完全表示／単発承認の内部設計、ポリシー選択と隔離・復旧の内部設計、受入条件の完成。342件の意味解析完了は要求しない。初期の依頼中許可範囲は台帳とcapability／tool_policyで明示し、未評価操作は単発経路で受入する。

## 11. 接続受入

| ID | 条件 |
| --- | --- |
| V06-01 | policy省略／null、表示のみ、各明示policy、未知値・無効値・旧profileの組合せ |
| V06-02 | 意味未評価の全引数を本人向けで確認し単発許可。依頼中許可は不可 |
| V06-03 | 通常公開操作の初回カード、秘密・未分類の非公開、長文全ページ、未知キー・数値・入れ子の無欠落 |
| V06-04 | guard選択時の禁止、未選択時への禁止・上流設定の非波及。並列の別クライアントでも同じ |
| V06-05 | 同じ会話でnone→guard→none、履歴維持、設定結果不明時のTurn開始0回 |
| V06-06 | 元call相違・policy binding相違・旧カード・別人・別会話・Steer／停止／取消競合 |
| V06-07 | catalog更新中の自動再評価と明示単発許可の競合。上流返信1回以下 |
| V06-08 | 202後の準備失敗、通信断、重複返信、Proxy／クライアント再起動、正式復元 |
| V06-09 | 旧DB行保持、新nullable scopeとpolicyを照合、未知世代を補完しない |
| V06-10 | Discord固有の型なしで他クライアントも同じ中核を利用。標準APIは新policy未選択 |

定義・JSON例・模擬試験・実Codex・実クライアント受入を分ける。本書の作成を試験合格や常駐反映として扱わない。


### フィールド上限と判定順の補足

profile・renderer・policy id・規則IDは最大128 ASCII bytes、versionは1以上の整数。policy一覧最大32、restrictions／upstream_overrides各64件、固定説明文は各1024 UTF-8 bytesまで。不透明binding／generationは既存IDと同じ8192 UTF-8 bytes以下。応答全体の上限は第12節のエンドポイント別表を優先し、超過時に配列を黙って省略しない。

意味未評価はpresentation.reasonにしない。readyならreasonとdiagnostic.codeはnull、未評価理由はsemantic_assessmentへ入れる。private_requiredのreasonはprivacy_unclassified／既存の秘密判定理由／display_too_large。unavailableではcall／完全性不足、policy禁止・検査不能、または表示不能の理由を返す。rendererはreadyでは必須、入口や取得不能ではnull可。

新規許可の競合判定は、本人認可・現在interaction→表示／scope／binding→適用中禁止条件→単発またはgrantの追加条件→送信意思commitの順。同じcallの送信意思が既にあれば既存結果へ照会する。どの失敗でも上流へ許可を送らない。拒否は表示／policy検査を通さず、既存の認可・revision・競合を確認する。

初期0.6には未指定実行へサーバー側で新policyを強制選択する機能を含めない。将来その機能を設ける場合は選択元・適用結果の明示と別の契約レビューが必要。ここでのpolicyはCodex本来の承認設定そのものを無効化するスイッチではない。

## 12. C06-01：エンドポイント別の応答上限（レビュー補完）

本節は従来の「各応答の総量は65536 bytes」という一律指定を置換する。全て**非圧縮のUTF-8 JSON本文全体**の上限。圧縮前提で超過を許さず、Gatewayは展開後の読取にも適用する。

| エンドポイント／応答 | 上限bytes | 件数・適用範囲 |
| --- | ---: | --- |
| GET /v2/codex/capabilities | 1048576 | 既存キーと新キーを含む全体。うちmcp_approval_v06単体は65536以下 |
| POST /v2/codex/responses の202、GET /v2/codex/responses/{id} | 262144 | 既存外枠とapproval_policyを含む単体。確定回答の本体取得は別の既存契約 |
| GET /v2/codex/interactions/{id}/presentation | 65536 | public/requesterの各ページ、入口・取得不能応答も含む |
| GET /v2/codex/interactions/{id}/operation | 65536 | 0.6のみ。超過する原引数はarguments_delivery=presentation_pages |
| GET /v2/codex/interactions/{id} | 262144 | 既存単体上限を継承。非MCP interactionも外枠上限は同じ |
| GET /v2/codex/responses/{id}/interactions | 67108864 | 最大256件、全件。ページング・黙示切捨てなし |
| GET /v2/codex/responses/{id}/mcp-grants | 1048576 | 最大16件、全件。ページング・黙示切捨てなし |
| 0.6のreply／grant取消／stopの応答および単体operation／stop照会 | 65536 | 当該制御操作の既存外枠。成果物本体・回答本体には適用しない |

旧0.3/0.4/0.5のoperation単体は従来の262144 bytesを維持する。旧Responseの一覧件数はそのprofileと当時の機能契約に従う。0.6 Responseは依頼中許可の選択・有効無効にかかわらず、履歴256件、未回答pending＋sending16件。ポリシー未選択のgrant一覧は空。既存workspaces/artifacts等のページングAPIへこの表の一覧上限を流用しない。

既存案のmcp_approval_v06.max_response_bytes=65536はpresentationの1応答上限を表す互換名として維持し、全エンドポイント共通とは解釈しない。

capabilityのmcp_approval_v06へ`response_limits`を必須追加する。JSON例の数値と本表を同値とし、Gatewayがコード内の一律64KiBで一覧を読む必要をなくす。未対応の上限を無制限へ拡張せず、対応するprofileを利用不可とする。

### 発行前の容量保証

単一値上限だけで登録可否を決めない。実際に送信するJSONのescapeと外枠、重複して入るscope／execution_policyも数える。

- 各policyの公開descriptor全体は2048 bytes以下。これがrestrictions／upstream_overridesの件数・各文字列上限より優先する。最大32 policyでもcapability追加部分全体65536以下、旧キー込み1048576以下を必須検証する。候補設定が超過なら新registryを公開せず、稼働中は直前の有効registryを保持する。既存のbinding／policy版を壊さない。
- execution_policy単体は将来状態を含め4096 bytes以下。Proxy生成のbinding／generationは128 ASCII bytes以下で発行する。将来の状態・理由・準備情報もこの予約内。要求由来の値を増やして既存bindingを拡張しない。
- grantは状態・失効理由・時刻・application_countの最大表現を含む予約サイズ49152 bytes以下。16件で786432 bytes、一覧の固定外枠予約8192 bytesを足しても1MiB以内となる。作成時に予約し、後続のapplication_count等は有界整数と固定理由のみ更新する。scope／policy条件は不変。
- 0.6 interactionは将来の終端／監査メタデータ込み196608 bytes以下で発行する。256件で50331648 bytes、外枠予約8192 bytesを足しても64MiB以内。Response単体も終端状態込み196608 bytesの予約を上限とし、262144 bytesの外枠内に収める。
- presentationはページ発行前に全scope／policy込み65536以内を検証。表示本文だけを数えない。operationは原引数をページへ分けてもメタデータが入らない場合、成功を装わず下記エラーとする。
- grant／interaction／Responseを新規発行するtransactionで、単体と一覧の両容量を検証する。停止・失効・取消で必要になるフィールド領域は先に予約し、後から取消の記録が容量不足になる設計にしない。上限超過を理由に過去grantを削除しない。

| HTTP／code | 場面と扱い |
| --- | --- |
| 422 approval_policy_metadata_too_large | 管理されたregistry登録候補が総量条件を満たさない。登録APIを持たない構成では同じ固定診断で設定を拒否 |
| 503 approval_metadata_capacity | 新Response／interactionの必要メタデータを予約不可。AI実行・新規許可を開始しない。受理済みの既存対象は照会・停止可能に保つ |
| 422 turn_grant_metadata_too_large | 新grantの必要条件を予約不可。grantも上流許可も確定しない。自動で単発へ変換せず、可能なら利用者が別キーで単発を明示選択できる |
| 503 approval_response_too_large | 不変条件違反・旧データ等で完全な応答を生成できない異常。部分200を返さず監査。小さなエラー本文を返す |

最後の503は正常な上限運用では発生させない。発行済みgrantの取消はgrant IDを使う独立APIであり、一覧の再取得成功を前提にしない。Gatewayは保存済みIDと元Responseへ拘束した取消を可能にし、一覧異常があっても「許可はない」と表示しない。全体停止も従来のstop APIで行える。新policyの登録変更で、古い発行済みgrantの取得容量を再計算して増やさない。

## 13. C06-02：ポリシー準備の期限・停止・復旧（レビュー補完）

### 13.1 準備期限と照会フィールド

初期版の準備上限は**60000ms**。起算点はResponseの永続受付commit。実行枠待ち、設定反映、Thread準備、設定の照合を含む。GET、再接続、worker再作成、Proxy再起動で延長しない。capabilityへpolicy_preparation_timeout_ms=60000を追加する。

全execution_policy／Response.approval_policyへ必須の`preparation`を追加する：

| フィールド | 型・意味 |
| --- | --- |
| started_at | RFC3339 UTC、受付時刻。不変 |
| deadline_at | RFC3339 UTC、開始＋60000ms。不変 |
| recovery_state | none / reconciling / fenced |
| turn_start_status | not_sent / intent_recorded / confirmed / unknown |
| configuration_isolation | not_required / pending / confirmed |

準備中の設定RPC送信とAIのturn/start送信を別の永続記録にする。turn_start_status=not_sentは、turn/startの送信意思がないことを正常なストアから確認し、古いworkerの送信権を失効させられる場合だけ返す。intent_recordedは送信した可能性がある境界。応答が失われたらunknown、Turnと開始が照合できたらconfirmed。フィールドを単なるHTTPタイムアウトから推定しない。

generationは設定準備確定時のみ発行し、不確定ならnull。configuration_isolationは不明な設定操作の遅着が次Runへ影響しない保証を表す。pendingのまま同じruntime／Threadを再利用しない。confirmedは旧workerの無効化・対象runtimeの隔離、または遅着がないことと実効設定の照合を完了した状態であり、設定RPCをキャンセルしたという要求だけでは足りない。

同一ホスト起動中は受付時のホスト単調時計の期限も永続記録し、停止中の経過時間を含めて残時間を求める。UTC期限との早い方で切る。壁時計後退・ホスト再起動などで残時間を証明できない場合は期限切れとして扱い、受付時から60秒を再付与しない。Gatewayはdeadline_atを待機表示に使えるが、自分の時計だけでResponseを終了扱い・占有解除しない。

期限は準備の成功からturn/start送信意思の取得までの待ちにも適用する。readyになっても期限を越えた未送信要求を開始しない。turn/startの送信意思を期限内に記録済みなら、以後は既存の開始結果照合・停止・UNKNOWN契約に移り、期限超過を未開始の証拠にしない。

### 13.2 状態、response.create operation、占有

既存のResponse phase／execution_statusを使い、新しいAI実行状態は追加しない。policy.reasonへpolicy_setup_timeoutとpolicy_setup_cancelledを追加する。失敗したpolicy状態は履歴として保持し、後からclosedへ塗り替えて理由を消さない。

| 状況 | policy.state／reason | Response phase／execution_status | response.create operation | 占有・次Run |
| --- | --- | --- | --- | --- |
| 受付後の準備・再起動照合中、Turn未送信確定 | preparing／null | acceptedまたはdispatching／not_started | acceptedまたはrunning | held。次Runは開始不可 |
| 設定確定、Turn送信意思前 | ready／null | dispatching／not_started | acceptedまたはrunning | held。停止・期限を次の境界で再確認 |
| 期限切れ・準備失敗、Turn未送信かつ設定隔離確定 | failed／policy_setup_timeoutまたはpolicy_setup_failed | rejected／not_started | failed、同じ理由code | released。次Runは新しい要求キーで、設定準備を独立実施 |
| 設定応答喪失、Turn未送信は確定、設定の遅着を隔離未了 | failed／policy_setup_unknown | unknown／not_started | unknown、policy_setup_unknown | held。AI未開始でも設定競合が未解消なので解除不可 |
| 上記から設定隔離完了 | failed／policy_setup_unknownを保持 | rejected／not_started | failed、同じ理由code | released。元要求は実行しない |
| turn/start送信意思があり開始結果不明 | ready／null（準備確定の履歴） | unknown／unknown | unknown、既存実行UNKNOWN理由 | held。Turn照合・停止を継続。再送しない |
| Turn開始確認済み | ready／null | started／in_progress | succeeded | held。既存の終端・停止契約へ |
| 準備中の停止、Turn未送信・設定隔離確定 | closed／policy_setup_cancelled | cancelled／not_started | failed、既存execution_cancelled | released。元要求の再開なし |
| 通常のRun終端 | closed／scope_ended | finished／実際の終端結果 | 開始結果を保持 | 既存契約で解放 |

期限に達していても設定隔離が未了ならpolicy_setup_unknown／heldを優先し、timeoutを理由に占有を解放しない。deadline_atは元の期限を保持する。状態保存が失敗した場合も、解放・取消完了を成功応答せず既存の保留を維持する。

phase=unknown／execution_status=not_startedは「AI実行が不明」ではなく「設定隔離が未完了で占有中」。preparation.configuration_isolationとpolicy.reasonで区別する。この状態をGatewayがAI失敗・AI実行中へ置き換えない。未知状態からの更新は同じResponse/bindingと保存した証拠で行い、新要求を作らない。

既にfailedで確定したpolicyは、その後の停止でもfailedと元reasonを保持する。停止意思・占有解放はResponseとstopで別に照会し、過去の準備失敗を消さない。

preparingから停止を受理したが設定隔離が未了の場合は、policy.state=closed／policy_setup_cancelled、phase=unknown／not_started、hold_state=held、response.createはunknownとする。停止意思は確定済みだが次Runは待つ。隔離後にcancelled／not_started、createはfailedへ確定する。停止で準備期限を延ばさない。

### 13.3 Turn ID発行前の停止

新しい停止APIは作らない。既存 `POST /v2/codex/stops`（Idempotency-Key必須）を使う。

```json
{"target":{"conversation_id":"conv_123","request_key":"original-create-key"}}
```

Response IDが分かっていれば `{"target":{"response_id":"resp_123"}}`。両方指定は不可。停止自身のキーと元実行要求キーを混同しない。照会は既存`GET /v2/codex/stops/{stop_id}`およびoperation照会。

停止意思commit、準備readyの公開、turn/startの送信意思取得を同じResponseの整合境界で直列化する。ネットワークRPC中はこのロックを保持しない。停止が先なら遅着設定成功でready／送信可能へ戻さず、Turnを開始しない。readyが先でもturn/start意思取得より停止が先なら同様。turn/start意思取得が先なら既存waiting_for_start／unknown／interrupt_pendingで照合し、開始前取消成功を断定しない。

Turn未送信かつ設定隔離確定なら200 cancelled_before_start。設定RPCだけが不明でTurn未送信が確定している場合は202 waiting_for_start、execution_status=not_started、interrupt_delivery=not_sent。ここでのwaiting_for_startは既存停止プロトコルの保留値で、AIの開始待ちを要求する意味ではない。Gatewayはpolicy準備状態と合わせて「停止を受け付けました。設定処理の終了を確認しています」と表示できる。隔離確認後にcancelled_before_startへ確定。stop operationのsucceededは意思保存だけであり、占有解放の保証ではない。

停止後も遅着した設定RPCの照合はできるが、入力・Turnを再送してはいけない。停止／拒否／取消は実行枠・準備worker枠とは独立して扱う。

### 13.4 Proxy再起動・設定反映応答喪失

- 起動時に旧workerの実行権を失効させ、同じResponse/binding／selection／deadlineを読み込む。選択をnullに置換しない。recovery_state=reconcilingとして照会できるようにする。
- turn/start送信意思なし、設定変更RPCも未送信、期限内なら、同じ要求の未送信準備を再開できる。別Responseや新しい期限を作らない。
- 設定変更RPC送信済みで結果未確定なら、同じ処理を再送せず照合する。policy_setup_unknownへ確定した元要求は、その後の照合で設定が正常と分かってもAIを開始しない。隔離後に失敗を確定し、利用者が次の依頼を出せる状態へ戻す。
- ready保存済みでもプロセス再起動で実効設定の証拠を失った場合は、同じbindingのpreparingへ戻して期限内に再検証する。新しい表示／grantはこの検証前に作らない。期限切れなら上表の失敗へ。これは再起動時の限定遷移であり、正常GETでreadyを準備中へ往復させない。
- turn/start送信意思があれば準備を再実行せず、既存の開始照合へ移る。期限切れでもexecution_status=not_startedへ戻さない。
- 正式復元の世代不一致では既存の実行保留を優先する。ストアの完全性を証明できず「送信意思なし」を根拠にできない場合はunknownを維持する。

### 13.5 追加受入

C06-S01：エンドポイント別の上限の直前・一致・1byte超過。非ASCIIとJSON escape、scopeとpolicyの重複も算入。
C06-S02：要素数と単一文字列が合法でもpolicy descriptor／registry全体が大きすぎる場合に、登録を拒否し直前版を維持。
C06-S03：16 grantを最大予約で作成し、失効・取消・件数更新後も全件取得可能。17件目は既存上限拒否。保存済みgrant IDの取消は一覧GET障害から独立。
C06-S04：256 interactionの全件取得と終端化、旧profileの読取上限を検証。
C06-P01：準備開始前・準備RPC中・ready直後・turn/start意思取得前後の停止競合。遅着成功から開始しない。
C06-P02：準備期限・GET連打・Proxy再起動・時計後退・ホスト再起動で期限が延びない。
C06-P03：設定だけ不明とTurn開始不明を別のResponse状態で返す。隔離前に次Runを開始しない。
C06-P04：同じResponse/bindingで未送信準備再開または結果照合。policy_setup_unknown確定後に元要求を実行しない。
C06-P05：202応答喪失時に元要求キーで停止し、同じ停止ID／operationから復旧。停止・設定失敗を利用者のMCP拒否と偽らない。
