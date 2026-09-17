# MCP承認0.5 — Proxy内部の処理・保存・競合設計

> 2026-09-17 責務再整理中：[汎用基盤とポリシー拡張の改訂](mcp-approval-layering-revision.ja.md)を参照。全ツールの意味解析を汎用単発承認の前提とする記述、および追加禁止を標準API全体へ適用する記述は見直し対象。本書をそのまま新規実装へ使わない。既存0.5 wireの後継差分は未確定であり、合意済みとして扱わない。

2026-09-17。接続仕様は合意済み。本文書は実装工程の入力であり製品コードではない。全ツールの意味評価が未完了の間は、本文書だけで実装Goとしない。

## 1. 既存構成からの変更単位

既存のService、approval_gate、Store、MCP call観測を使い、0.3/0.4の応答形式を保持する。0.5はResponseに固定されたprofileで分岐し、capabilityの再照会で既存Responseのprofileを変更しない。

| 実装単位 | 入力・出力と責務 |
| --- | --- |
| catalog | runtime/Thread/configごとの共有取得。状態付き定義参照、対象toolの不透明世代を返す。HTTP接続の寿命から独立 |
| registry | 信頼する接続・評価済み定義・静的な規則IDを照合。上流descriptionを実行時の許可規則にしない |
| evaluation | VerifiedCall、定義参照、必要な対象データ根拠から、作用・表示先・条件付き許可範囲・完全な表示項目を評価 |
| presentation_v05 | 評価結果を上限付きの不変ページへ分割。audienceごとの表示ID・ページtoken・期限を発行 |
| grant_policy | 単発とturn_toolを分離。作成時と全後続callに同じ評価器を使い、既存grantとの一致を判定 |
| refresh_wait | 定期catalog更新待ちのcall ID参照と期限だけを管理。正常更新完了後に再評価 |
| approval_application | 手動許可・拒否・自動適用・失効の確定点。送信意思の永続化後にのみ上流へ返信 |

JSONを複数箇所で独立に解釈せず、正規化済みの型を内部で受け渡す。APIのwire enumと内部状態を区別する。秘密本文を含む型へ汎用Debug／エラー丸ごと表示を付けない。

## 2. 型と評価の不変条件

### VerifiedCall

安定call ID、Thread/Turn/Response、実引数の不変参照、入力世代、取得時刻・期限、実接続の識別を持つ。承認フォームとの対応付けが未確認なら作らない。同じcall IDで実引数が変わった場合は古い参照を失効させる。フォームの説明文から欠けた引数を補完しない。

### EvaluationBasis

VerifiedCall、catalog epoch、tool定義世代、policy世代、実効設定世代、必要なら対象本文／schemaの識別・版・完全取得の根拠を持つ。対象本文に依存しない操作は根拠不要と明示し、将来Notion部分置換を許可対象へ広げる際には省略不可。現版のupdate_contentは対象本文の読取りへ進む前に実行不可と判定する。本文根拠の取得経路・版固定が未確認な規則を、通常writeとして登録しない。

### EvaluationResult

- Complete：作用集合、public/requesterの判定、表示項目全体、許可対象・個別確認対象の操作集合、制約を持つ。
- Waiting：catalog取得等の理由。許可可能な表示tokenを持たない。
- Unavailable：型・意味・根拠・表示の完全性不足。理由だけを返し、有効な許可tokenを作らない。

Completeは「安全」の意味ではない。任意コードの実行を毎回確認する場合も、確認するコード全文と固定された実行先を完全に示せればCompleteとなる。用途や作用を確定できない任意の入力を、同じ分類へ無条件で移さない。

公開可否、操作内容の完全性、依頼中許可適格性は別の値。作用にexecute/credential/external_send/permission_change/delete/unknownが含まれる呼出しへturn_toolを適用しない。allowed_effectsに入ることだけでは足りず、ツール別の条件、scope、各世代、期限も一致させる。

### 全引数の扱い

実定義の全キーを列挙し、指定値・null・未指定を区別する。既定値は意味を確認したものだけ補完。未知キー、型違反、未評価enum、内包入力の未評価要素は読み飛ばさない。cursor等の非表示項目も実引数fingerprintに含める。プロトコル値というラベルだけで作用のある引数を隠さない。

[引数ラベル表](mcp-approval/argument-labels.json)は436種類の共通名とtool別上書き、[台帳](mcp-approval/tool-inventory.json)は342操作・1,082引数の表示名を持つ。表示名の付与は意味評価やfixture完成の代わりではない。ラベルは規則から引き、上流の未評価descriptionを翻訳して動的に置換しない。

## 3. GETから表示発行まで

1. 認証・instance/recovery・workspace・Response profile・対象interactionを確認する。
2. catalog参照を取得する。未取得なら共有workerへ要求して最大250ms以内にWaitingを返す。DB transactionやapproval_gateを保持してRPCを待たない。
3. 必要な不変参照を採取し、重い構文解析・ページ分割・HMAC対象の組立てをロック外で行う。
4. approval_gate→DB transaction→必要な短時間メモリロックの順で、採取時と現在のcall/revision/scope/世代/状態/期限を再照合する。
5. 同内容・同audienceの有効版があれば再利用する。新しい版なら、旧版の失効と新メタデータを同じtransactionへ保存する。
6. commit成功後だけ有効tokenを返す。commit失敗時は候補ページを破棄し、許可可能な応答を返さない。メモリ登録失敗時もDBだけから本文を再生しない。

DB commit後、応答前にプロセスが落ちた場合は、再起動で旧版を失効させる。同じGETを再実行して上流callやAI実行を再送しない。unavailableの照会を繰り返して表示4版を消費しない。

## 4. ページとtoken

表示全体は不変のsnapshot。publicは1ページ、requesterは最大64ページ。ページごとに規則を再評価して異なる版の本文を混ぜない。全ページの内容・順序・件数、audience、scope、revision、callの実引数をプロセス鍵付きfingerprintへ拘束する。

tokenはOS乱数による256bit以上の不透明値。ページを読み直して新しいtokenを発行しない。既存版のindex指定は同じページを返す。範囲外・別版・別audienceを拒否する。本文・無鍵の本文digestを永続化しない。

一つの長い値は全文を連続した断片に分け、項目名・順番を明示する。構造式は親子関係・否定・AND/ORを各断片で追えるようにする。表示する制約文も上限に算入し、入りきらない制約を落とさない。どう分割しても意味が欠ける場合はUnavailable。GatewayのDiscord投稿への再分割はGateway責務。

メモリには生引数・派生本文を重複コピーし続けず、所有権付きの不変参照を用いる。元call期限、表示最大10分、audienceごとの最大4版を超えて保持しない。秘密の永続キャッシュは作らない。

## 5. 返信の確定点

同一キーの既登録operationを最初に照会する。結果があれば、新しい表示で同じキーの意味を変えず従来結果を返す。

新規許可はAPI契約のexpected_*、全ページtoken、audience、actions、本人のscopeを検証する。拒否は現在のinteraction/revisionと認可・競合だけを検証し、表示の取得や全ページを要求しない。

ロック外で行った評価は、そのまま送信根拠にしない。approval_gateとDB transaction内で全EvaluationBasisの有効性、期限、停止・Steer・取消、catalog状態、grant状態、既存の送信意思を再確認する。既存の上流送信意思を一度だけ保存し、commit後に上流応答する。DB書込失敗なら応答しない。

送信結果不明はunknownとして照会へ移り、リトライで上流へ再送しない。自動適用はこの確定処理を共用し、特別な省略経路を作らない。送信意思保存がStopより先なら「取り消した」と言わず、上流Turn停止の既存境界に従う。

## 6. 正常更新中の待機と起床

refresh_waitはcall ID・interaction ID・grant ID・待機理由・元期限を参照する。生引数・本文を複製しない。未回答interaction上限16件に含め、別キューへ逃がさない。

catalog workerの完了／失敗、Stop/Steer/revoke、grant期限、interaction期限で対象を起こす。通知がなくても期限監視で回収する。正常更新なら最新根拠で再評価し、第5節の確定点へ進む。期限を迎えた待機へ遅着結果を適用しない。

catalogの期限とgrantの期限は別。単にgrantが失効しても共有catalog workerを失敗にしない。失敗後の古いgrant復活は禁止。availabilityはその時点のcatalog状態とgrant状態から導出し、DBへ保存したrefreshingを再起動後の事実として使わない。

## 7. 保存形式と移行

現行Storeはschema=2、records(kind,id,value)とoperationsを持つ。0.5導入ではschema=3へ一度のtransactionで移行し、旧バイナリによる誤解釈を拒否する。旧バイナリはschema=2以外を拒否する現状を維持。単にJSON項目が増えただけとしてschema=2に据え置かない。

| 保存対象 | 保存する内容 |
| --- | --- |
| response | 既存approval_presentationに固定profile。既存の宣言なしは0.3、modeのみは0.4として解釈する。0.5へ書き換えない |
| interaction | 0.5用のoperation監査メタデータ、全scope、過去の評価、使用grant ID。生引数を含めない |
| mcp_presentation_v05 | 表示ID、audience、call/interaction参照、scopeと世代、revision、fingerprint、ページ件数・token、発行期限、失効状態。本文なし |
| mcp_grant | 0.5だけprofile/grant_policyを必須追加。条件は作成時の不変値。availabilityは照会時に算出する |
| operation | 既存の冪等処理と上流送信意思。0.5返信の全照合値をbody fingerprintへ含める。生本文を保存しない |

recordsのkind追加を全件走査・保持期限・backup/restoreへ反映する。旧0.3/0.4の履歴を0.5型で読まず、削除しない。未知schemaは受付前に拒否。移行途中の失敗はrollback、再実行は一度だけ完了する。移行後のDBを手作業で2に戻すことは禁止。旧版へ戻す必要があれば正式backup/restore手順で世代を更新し、結果不明の実行・配信を再送しない。

起動時は全0.5表示を無効化し、active/pending grantを失効させる。上流返信のunknown監査は保存し、未実行や成功に変換しない。秘密メモリとHMAC鍵を復元しない。Gatewayだけの再起動ではProxyの有効なcall/grantを失効させない。

## 8. 設定の互換

mcp_turn_grant_toolsは「省略」と「明示空map」を読込み時から区別する。内部のoption型またはpresence情報を保持し、Defaultの空mapへ早期変換しない。

- 0.3/0.4：省略も空mapも従来の許可対象なし。明示mapだけ従来評価。
- 0.5：省略は評価済み通常policy、空mapは依頼中許可なし、明示mapは評価済み通常policyとの積集合。

設定変更は世代変更として既存grantを失効させる。省略形へ移す本番差分は実装・受入後の配備工程で提示する。設計作業中には変更しない。

## 9. 内部受入

| ID | 検証する境界 |
| --- | --- |
| PI-01 | 全342定義のキー・必須型・表示名を照合。未知キー・enum・schema変更を拾う |
| PI-02 | 待機／取得失敗を未対応・機密理由へ変換しない。同一定義更新で追加クリックなし |
| PI-03 | ページ全体HMAC、順序、重複token、他audience、末尾制約文の欠落、長い単一値 |
| PI-04 | ロック外評価後のcall変更・世代変更・Stop・Steer・revoke。古い根拠から送信0回 |
| PI-05 | DB commit前後・上流送信前後の停止。手動と自動の送信意思は合計1件以下 |
| PI-06 | schema2→3の失敗・再開・未知版拒否、旧履歴不変、旧バイナリ拒否、正式復元 |
| PI-07 | ソース本文・生引数・非公開本文・無鍵digestがDB／ログ／一時ファイルに残らない |
| PI-08 | 設定省略／空map／明示mapと0.3/0.4/0.5の全組合せ |
| PI-09 | Notion update_contentの実行不可を単発／自動適用の両確定点で維持。NR-01〜07も確認 |

受入ケースの定義は製品テストの成功ではない。Gateway G05、API F、カタログCと合わせて実装後に実行する。全ツールの条件式・fixtureと、対象本文に依存する操作の未確定事項は個別設計で完成させる。

## 10. 実行制御と個別の複合入力

[上流実行制御](mcp-approval-execution-boundary.ja.md)により、表示を経ない承認の迂回を防ぐ。Notionの部分置換禁止は更新ツールのprompt/user条件とProxyのcommand別判定を組み合わせる。UI profileの違いで禁止を解除しない。現行のMCP設定差分検知にはAppsが含まれないため、実効設定の同期・読戻しも変更対象。これは実装設計であり常駐設定を変更した記録ではない。

[GitHubツリー](mcp-approval-github-tree-design.ja.md)と[Driveコメント](mcp-approval-drive-comments-design.ja.md)に具体入力・完全表示・複合作用を定義した。29例は設計fixture。静的schema検証だけでは意味検証を代替できない。

責務整理合意後の接続先は[API 0.6レビュー案](mcp-approval-api-v06.ja.md)。本書の技術的知見は引き継ぐが、単発承認への意味評価必須条件・全実行への禁止波及・全342件の着手ゲートは後継要件に従って置き換える。
