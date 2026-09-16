# MCPインライン承認 API — 0.4接続レビュー案

2026-09-16。**接続合意版0.4（R-01解消、2026-09-16 Gatewayレビュー完了）・実装／受入／配備は別工程。** [0.3](mcp-turn-approval-api.ja.md)の追加契約。既存0.3の常駐機能を変更済みとは扱わない。Gatewayの `docs/mcp-inline-approval-change-request.ja.md` に回答する。利用者が「検索語・アクセス先URLなどの操作対象を元の会話に表示する」と決定した内容を反映済み。公開方針・接続レビューは完了済み。Proxyの実装・検証状況は[検証記録](mcp-inline-approval-validation.ja.md)で別途管理する。

## 1. 目的・責務

依頼元の会話に、判断可能な操作内容と「この依頼中、このツールを許可」「今回だけ許可」「拒否」を同じカードで提示する。ターン許可対象外では許可と拒否の2択。許可一覧・取消は `/mcp` に分け、承認カードに紛らわしい一覧ボタンを置かない。DMへの移動は行わない。常時許可されている上流ツールに新たな確認を発生させない。

- Proxy：実呼出しとの対応付け、公開情報の生成・公開可否・承認に十分かの判定、表示トークンの照合、許可・送信・失効。
- Gateway：認証済み本人と投稿先の照合、Discord表示、表示版の永続化、ボタン押下の認可、冪等照会・配信復旧。
- モデルの説明は任意の参考情報。実操作から生成した説明と混ぜず、公開可否・承認可能性の根拠にしない。本APIはモデル説明を返さない。

`source_conversation` はインターネット一般への公開許可ではない。依頼元会話の閲覧権限を持つ人が読め、投稿後の削除でも既閲覧を取り消せない。ProxyはDiscordの参加者・権限を把握しないため、実投稿先と本人の認可はGatewayの責任。

## 2. 互換性とCapability

既存 `GET /interactions/{id}/operation` の `arguments` と `disclosure=requester_only` は変更しない。これを公開表示へ転用しない。新APIの接頭辞も `/v2/codex`。既存認証・世代ヘッダー・workspace認可・復元保留を継承する。

`GET /capabilities` に次を追加する（トップレベル）。

```json
{
  "mcp_inline_approval": {
    "enabled": true,
    "profile": "source-conversation-v1",
    "max_response_bytes": 32768,
    "max_display_text_utf16_units": 1400,
    "max_display_fields": 8,
    "max_presentations_per_interaction": 4
  }
}
```

すべて必須。enabledはboolean、profileは上記固定string、上限は非負integer。未知profile・欠落・無効では0.3の本人限定フローを維持する。新APIへの404を理由に実引数を公開しない。既存2 capabilityも独立に検証する。

追加の全体ON/OFF設定は設けない。実装後は既存 `mcp_turn_approval_enabled` に従って本capabilityを提供する。ただしenabledは「任意の引数を公開可能」の意味ではない。具体的な公開可能性は呼出しごとに判定する。0.3のターン許可allowlistは公開許可リストではない。

## 3. 呼出しごとの表示先指定

新クライアントはResponse受付に次の省略可能なフィールドを追加する。

```json
{
  "input": "依頼本文",
  "interaction_capabilities": ["mcp_form"],
  "approval_context": {"principal_id":"opaque-user","channel_id":"opaque-thread","run_id":"run-example"},
  "approval_presentation": {"mode":"source_conversation"}
}
```

指定時は有効なapproval_contextが必須。modeはsource_conversationのみ。未知の値・型・余計なキーは400 invalid_approval_presentation、未対応／無効は503 inline_approval_disabled。受付の冪等fingerprintに含め、Response内で不変。同じconversationの次Responseへ暗黙継承しない。省略したResponseには公開用本文を発行せず0.3を維持する。

この指定は、認証済み利用者について「検索語・アクセス先URLなどの操作対象を元の会話へ表示する」という方針が成立していることを、信頼されたGatewayが宣言する。今回の利用者は同方針を明示承認済み。一般配布先ではGatewayの導入説明・公開方針確認に含める。毎回の追加ボタンやProxyの追加ON/OFF設定は設けない。ツールが後から得た秘密情報すべての公開への同意ではない。channel_idは既存contextと同じ不透明IDで、Discord IDをProxyへ追加送信しない。

## 4. 公開用表示の取得

`GET /interactions/{interaction_id}/presentation`。認証・認可のあるクライアント専用であり、公開HTTPエンドポイントではない。以下はHTTP 200の成功例。通常のページ内検索を最初のカードで承認できる表示を例にする。

```json
{
  "interaction_id":"int_example",
  "response_id":"resp_example",
  "turn_id":"turn_example",
  "revision":1,
  "scope_fingerprint":"opaque-call-binding",
  "presentation_id":"present_example",
  "presentation_fingerprint":"opaque-presentation-binding",
  "profile":"source-conversation-v1",
  "renderer":"browser-find-v1",
  "audience":{"kind":"source_conversation","channel_id":"opaque-thread"},
  "state":"inline",
  "reason":null,
  "expires_at":"2026-09-16T12:10:00Z",
  "display":{
    "disclosure":"source_conversation",
    "provenance":"proxy_verified_call",
    "title":"ページ内を検索",
    "fields":[
      {"label":"操作","value":"現在のページのアクセシビリティ情報を文字列検索します"},
      {"label":"対象","value":"このMCP接続で現在開いているページ"},
      {"label":"検索語","value":"ノートパソコン"}
    ],
    "limitations":["現在のページのURLは引数になく、並列操作で対象ページが変わる場合があります","この依頼中の同じツールを別の引数でも許可します。サイト限定ではありません","操作内容の説明であり、安全性を保証するものではありません"],
    "omissions":[]
  },
  "actions":{"allow_once":true,"allow_turn_tool":true,"decline":true,"open_private_details":false}
}
```

### 型・状態

上記の全フィールド必須。ID・トークンは非空string（最大8192 UTF-8 bytes）、revisionは1以上のinteger。turn_id／scope_fingerprintは対応付け不能時にnull。presentation_id／presentation_fingerprint／expires_atは版を発行できないunavailableまたはpresentation_limit時にnull。それ以外の有効な版では必須の非null値。rendererは対応するアダプタがない場合、または実操作を公開表示へ投影できない場合にnull。reasonはinline時だけnull。audience.channel_idは最大128 UTF-8 bytes、Responseのcontextと一致。scope全体は0.3のGET operationで取得し、新しい表示版と照合する。**display以外のフィールドはカード本文へ転載しない。**

| state | 許可される動作 |
| --- | --- |
| inline | 公開情報だけで判断できる。allow_once=true。allow_turn_toolは0.3の現在の適格性に従う |
| private_required | 許可ボタンなし。理由を公開カードに表示し、本人限定補足確認と拒否だけを提供 |
| unavailable | 操作情報を確認できない。許可も補足による推測許可も不可。pendingであれば拒否可能 |

actionsの値はすべてboolean。declineもinteractionがpendingの場合だけtrue。private_requiredはallow_once=false／allow_turn_tool=false／open_private_details=true。unavailableは前記3値をfalse。閉じたinteractionではすべてfalse。

reason列挙：private_arguments / unsupported_renderer / unknown_arguments / information_incomplete / display_too_large / operation_unavailable / interaction_closed / presentation_limit。inlineではreason=null。未知state・reason・renderer、必須項目の欠落、不整合なactionsでは許可しない。rendererは認識済みの固定テンプレートIDでありMCP自己申告を使わない。未知rendererの内容を見てGatewayが独自に承認可能と判定しない。

displayはtitle（非空string）、fields（label/valueが非空stringの配列）、limitations（非空stringの配列）、omissions（列挙string配列）。provenanceとdisclosureは例の固定値。omissionsはprivate_arguments / unknown_arguments / information_incomplete / display_too_largeのみ。**omissionsが1件でもあればinline不可**。公開してはならない値そのもの、キー名、パスを省略理由へ埋め込まない。

private_requiredの例（その他のIDは成功例と同様、renderer=nullを許す）：

```json
{
  "state":"private_required",
  "reason":"private_arguments",
  "display":{
    "disclosure":"source_conversation",
    "provenance":"proxy_verified_call",
    "title":"操作内容の補足確認が必要です",
    "fields":[],
    "limitations":["公開できない情報を含むため、本人限定画面で確認してください"],
    "omissions":["private_arguments"]
  },
  "actions":{"allow_once":false,"allow_turn_tool":false,"decline":true,"open_private_details":true}
}
```

この例は状態差分の抜粋。取得不能ならprovenanceを `unavailable` とする（provenanceはproxy_verified_call/unavailableの2値）。未知の実操作をverifiedと表示しない。

## 5. 決定済みの公開方針と対応範囲

利用者は次を明示選択した：**検索語・アクセス先URLなど「何を対象に操作するか」を元の会話の承認カードへ表示する。認証情報・秘密入力・任意の実行コードは公開せず、本人限定補足へ分ける。会話の閲覧者には操作対象も見える。** これにより、自由文であることだけを理由に検索を常にprivate_requiredへ戻す初稿の扱いを廃止する。

公開は下記rendererで明示した操作対象に限定する。全引数・モデル文面・ツール結果を公開する許可ではない。対象が業務上の名前や私的な検索語であっても、明示された対象公開方針の範囲では表示される。認証情報などは別途除外する。任意の文字列が秘密であるかを完全に推定できる保証はしない。秘密を操作対象の自由文に偽装したすべての場合まで防げるとは記載しない。

### 5.1 rendererの一覧

公開用rendererは評価済みMCPの実定義と意味に対応する専用アダプタとする。接続先・実定義の確認結果を設定世代へ結び付ける。MCPのdescriptionやreadOnlyHint、ツール名だけで採用しない。登録済みplaywrightのbrowser_find（textかregexの一方）とbrowser_navigate（urlのみ）、browser_tabs（actionと任意のindex/url）の公開定義を文書改訂時に確認した。実接続の意味・定義一致と異常系は実装後の受入で検証する。

| renderer | 引数の受入形・公開表示 | 公開ボタン |
| --- | --- | --- |
| browser-find-v1 | browser_find。textまたはregexの一方のみ、非空string。操作種別・現在ページが対象であること・検索語または正規表現の全体を表示。未知キー、両指定、null、型違いは補足へ | 通常はinline。単発＋拒否、0.3の適格性がtrueならターン許可も表示 |
| browser-navigate-v1 | browser_navigate。urlだけのobject、絶対HTTP(S) URL。スキーム・ホスト・ポート・パス・query・fragmentを含む実引数のURL全体を表示。ページ移動が起きることと、リンク先の安全性・リダイレクト先を保証しない旨を併記 | 通常はinline。ターン許可可否は別判定。現在の常駐allowlistでは単発＋拒否 |
| browser-tabs-list-v1 | browser_tabs。action=listのみ、その他引数なし。固定の「このMCP接続のタブ一覧を取得」。タブ名・URL・結果はカードに追加しない | listの実定義一致を検証して提供。上流承認が発生した場合のみ |
| なし | click、入力・送信、ファイル操作、その他未評価ツール | 操作対象の公開方針は適用可能だが、0.4初期実装では未知形式を推測せず本人限定補足。追加rendererは後続の文書改訂と受入を要する |
| なし | browser_evaluate／browser_run_code_unsafe | 実行コードは公開しない。本人限定の単発確認のみ |

browser_findはWeb全体の検索ツールではなく現在ページの検索。現在のURLが引数にない場合、過去のnavigateやモデル説明から補って表示しない。ページURL・タブIDが不明でも「この接続の現在ページ」という実ツールの操作範囲を全体として示し、特定サイトに限定した許可と誤認させない。並列のブラウザー操作等で現在ページが変わり得ることを制限に表示する。特定ページへの固定を保証するAPIではない。

すべてのrendererで、実引数の未知キー・型違い・対応付け不能・0.3のredacted_paths・秘匿判定・サイズ超過があればinline不可。APIで操作対象を表示してよいことと、その操作が安全であることは別。ターン許可allowlistは変更しない。上流常時許可のツールを試験のために承認必須へ変更しない。

### 公開fieldsとactionsの固定契約

renderer IDの集合は `browser-find-v1` / `browser-navigate-v1` / `browser-tabs-list-v1`。単に同名ツールが存在するだけで別実装に同じrendererを割り当てない。Gatewayはこの集合を認識し、本文の文字列内容を安全性判定に使わず、型・全ID・上限・state/actionsを検証する。

| renderer | title | fields（順序固定） | 必須limitations |
| --- | --- | --- | --- |
| browser-find-v1 / text | ページ内を検索 | 操作＝現在のページのアクセシビリティ情報を文字列検索します、対象＝このMCP接続で現在開いているページ、検索語＝textの原文全体 | URLは引数になく並列操作で対象が変わり得る。安全性保証ではない |
| browser-find-v1 / regex | ページ内を正規表現で検索 | 操作＝現在のページのアクセシビリティ情報を正規表現検索します、対象＝このMCP接続で現在開いているページ、正規表現＝regexの原文全体 | 同上。Proxy側ではregexを実行しない |
| browser-navigate-v1 | 指定URLへ移動 | 操作＝このMCP接続のブラウザーでページを開きます、アクセス先URL＝urlの原文全体 | ページ移動を伴う。リンク先の安全性や移動後URLは保証しない |
| browser-tabs-list-v1 | ブラウザーのタブ一覧を取得 | 操作＝タブ一覧を読み取ります、対象＝このMCP接続のブラウザー | 操作説明は安全性保証ではない |

allow_turn_tool=trueの表示にはさらに「この依頼中の同じツールを別の引数でも許可します。サイト限定ではありません」をlimitationsへ必ず含める。字句を縮めて上限を回避しない。Gatewayは同一カードに失効条件も表示する。検証されたツール種別はtitle/fieldsで伝え、非公開のserver設定値や接続URLを追加で転載しない。

HTTP応答のactionsは次のとおり。上流で承認が不要ならinteraction／presentation自体を新設しない。

```json
{
  "inline_once_only":{"allow_once":true,"allow_turn_tool":false,"decline":true,"open_private_details":false},
  "inline_turn_eligible":{"allow_once":true,"allow_turn_tool":true,"decline":true,"open_private_details":false},
  "private_required":{"allow_once":false,"allow_turn_tool":false,"decline":true,"open_private_details":true},
  "unavailable_pending":{"allow_once":false,"allow_turn_tool":false,"decline":true,"open_private_details":false},
  "closed":{"allow_once":false,"allow_turn_tool":false,"decline":false,"open_private_details":false}
}
```

上記はactionsのケース一覧で、APIがこの一覧objectを返す意味ではない。navigateでターン許可を出すためにallowlistを拡張しない。browser_findも現在のscope・秘匿・停止・期限等により毎回再評価する。

### 5.2 認証・秘密・コードの除外

秘密判定はProxyのバージョン付き規則として行い、Gatewayの文字列置換に委ねない。判定で1件でも該当すれば、**当該呼出しの対象値を一切公開せず**private_requiredとする。URLのtokenだけ伏せて残りを表示し、許可を求める方式にはしない。本文・reason・omissionsにも該当値を入れない。

最低限、次を検証対象とする（規則の版はrenderer／公開方針世代に拘束）。

- 既存redacted_pathsが非空、秘密入力／認証／permissionsと識別された要求、未知の引数キーは公開しない。内部フォーム・URL認証は本APIへ変換しない。
- `authorization`、`proxy-authorization`、`cookie`、`set-cookie`、`password`、`passwd`、`secret`、`client_secret`、`token`、`access_token`、`refresh_token`、`id_token`、`api_key`、`apikey`、`credential`、`signature`、`sig`、`session`、`sessionid`、`code` を認証候補名とする。ASCII大小文字を区別せず、ハイフンとアンダースコアを同一視する。URLのquery名にあれば値が空でも補足へ。X-Amz-／X-Goog-で始まる署名付きURLのqueryも同様。
- URL userinfoは常に補足へ。HTTP(S)以外、相対URL、解析不能、制御文字、不正なpercent encoding、正規化で対象を曖昧にする入力も補足へ。fragmentもquery形式・認証候補名を検査する。表示前にURLを取得したり、短縮URLやリダイレクトを解決したりしない。
- 検索語・regex・URL内の認証候補名と `:`／`=` で結ばれた値、Bearer／Basicの認証ヘッダー形式、PEM秘密鍵ヘッダー、JWT形式の値、`sk-`／`ghp_`／`github_pat_`／`xoxb-`／`xoxp-`等の既知資格形式は補足へ。誤検出時も勝手に公開して回避しない。
- percent encodingは検査用に最大2回まで復号し、二重化・復号後に残る有効なpercent escapeや入れ子のURLなど、同規則で単純に検査できない入力は補足へ。HTML entity／JSON escape等の埋込みにより解釈が曖昧な入力も補足へ。検査用変換で元の表示内容を置換しない。
- regexをProxyが実行して安全判定することはしない。任意コードを受け取るツールは全体を本人限定にする。検索値にコードらしい文字列があっても「実行してよい」と解釈しない。本変更で任意コード実行rendererは作らない。

一般的な自然文へ任意に埋め込まれた未識別の秘密、独自形式・暗号化された秘密を完全に検出するものではない。既知の検出範囲と限界を運用マニュアルに記載する。公開に同意した操作対象の通常値まで「秘密でないと証明できない」だけで一律補足に戻さない。

### 5.3 例

| 実引数の例 | 結果 |
| --- | --- |
| browser_find `{"text":"ノートパソコン"}` | 検索語を表示、inline |
| browser_find `{"regex":"link \"[1-5]位"}` | 正規表現として元の値を全表示、inline（表示上限内） |
| browser_navigate `{"url":"https://example.com/search?q=notebook#results"}` | URL全体とページ移動を表示、inline |
| URLにuserinfo／access_token／署名queryが含まれる | 対象値を公開しない、private_required |
| 検索語に `password=...`／Bearer資格値が含まれる | 対象値を公開しない、private_required |
| browser_evaluateにfunctionがある | 実行コードを公開しない、本人限定の単発確認 |
| 対象が表示上限を超える／必要な引数が欠落 | 切り詰めて承認させず、補足またはunavailable |

## 6. 表示版・トークン・保持

presentation_fingerprintは秘密引数のハッシュではない、暗号学的に予測困難な不透明トークン（256bitのランダム値）。以下の不変スナップショットに対応する：元call、scope_fingerprint、interaction/revision、Response/Turn/context、入力・設定・復元世代、audience、rendererとその版、display全体、state/reason/actions、期限。

同じ有効内容のGETは同じpresentation_idとトークンを返す。内容、対象、公開方針、操作可否が変われば別版とし、**旧版を失効させる**。同じinteraction/revisionのまま表示変更しても旧トークンでは返信不可。表示の変更は許可scopeの拡大を意味しない。scope_fingerprintだけで公開表示の同一性を代用しない。

発行はGET内のDBトランザクションで記録する。監査保存不能なら503、承認可能なトークンを返さない。1 interactionの発行は最大4版（同内容照会は増えない）。超過時は旧版失効のうえpresentation_limitで本人限定経路へ戻す。基礎callも取得不能ならunavailable。DBには表示本文・生引数を保存せず、ID、トークン、scope参照、状態、renderer版、公開方針世代、期限とHMAC-SHA256の内容識別値を記録する。HMAC鍵はプロセス内のみ。低エントロピーの非公開情報の単純hashを永続化／公開しない。

表示本文はcallと同じ上限付きメモリで保持し、callとともに破棄。最長10分、元interactionとcallの残存期限を超えない。再GETで期限を延長しない。単調時計で後退延長を防ぐ。再起動・正式復元・上流接続喪失・イベント欠落・stop・Steer・終端・設定変更で旧版失効。再起動後に記録だけから表示を復元して許可しない。

監査メタデータとreply冪等記録は基本契約の `until_explicit_state_retirement`。期限切れ表示本文は保持しない。Gatewayも生引数・公開値の写しを通常DBやログへ追加保存せず、ID／トークン／表示識別値とDiscord message ID、配信状態を記録する。公開メッセージ自体の保管・閲覧はDiscordの範囲で、Proxyの10分保持による削除保証はない。

## 7. 直接ボタンからの返信

0.3のreplyへ以下2フィールドを追加。既存のscope照合・冪等性・送信結果の意味は変更しない。

```json
{
  "expected_revision":1,
  "expected_scope_fingerprint":"opaque-call-binding",
  "approval_view":"source_conversation",
  "expected_presentation_fingerprint":"opaque-presentation-binding",
  "response":{"action":"accept","content":{}},
  "grant_scope":"turn_tool"
}
```

単発はgrant_scopeを省略する。新フィールドは必ず対で指定。approval_viewはsource_conversationのみで、未知値・片方だけ・null・空文字は400 invalid_approval_presentation。対象Responseが本表示方式を宣言していない場合は409 presentation_context_mismatch。declineは既存0.3のrevision付きbodyを使い、表示トークン不要。拒否を許可へ変換しない。

Proxyは返信意思保存と同じ直列化境界で、最新の表示版と有効期限、call・scope・公開方針、state=inline、選択したaction=true、停止・世代・interaction状態を検証する。チェックと送信意思保存の間に公開方針更新や失効が割り込まない。**新フィールドを受け取って単に無視する実装は禁止**。Gatewayの再GET確認に加えProxyも照合する。

同一キー・同一bodyの既登録返信は、旧表示が後で失効していても元operationを返す（新規送信しない）。未登録要求は現在の照合を通す。初回返信と自動適用、ターン許可の発効・取消・TTLは0.3のまま。操作照会で結果不明の返信を解決し、別キーで送信し直さない。

本人限定補足後は0.3のexpected_scope_fingerprint付き返信を使用する。source_conversationトークンを流用してprivate_requiredを通さない。既存クライアントの0.3返信は維持し、Proxyが表示を見たという証拠にはしない。これらは信頼されたGatewayのAPIであり、悪意あるBearer所有者のUI迂回まで防ぐ認証方式ではない。

## 8. Gatewayの表示・配信状態

1. capability、context、現在のinteraction／operation／presentationを取得し、全ID・revision・scope_fingerprintと既存scopeを照合する。
2. 表示スナップショットとボタンの対応をDBへ先に固定する。本文は公開displayからだけ構成し、モデル説明や私的argumentsで補完しない。
3. 同じ1メッセージに説明とボタンをまとめて送る。最初にボタンだけ送ったり、説明を後続投稿で足したりしない。送信中には押下が到着し得るため、Gateway側で配信確定まで承認処理しない。
4. Discordが返したmessage IDと対象会話を確定し、保存済み版に結び付けて押下を有効にする。送信結果不明なら照合復旧し、別カードを無条件に再投稿しない。
5. 押下で本人・Guild／会話・message ID・版・Runを検証し、Proxyの現状態と再照合する。未知・古い版の押下を新しい表示に自動的に移さず、再表示して選び直してもらう。
6. 新しい版を表示する前にGateway側で旧版を無効化する。Discord上のボタン除去が失敗しても旧押下を受理しない。拒否、停止、承認完了も同様。

カード文言：通常は操作内容＋制限、対象ツールだけ3択、それ以外は2択。ターン許可には「この依頼の同じツールを別の引数でも許可。追加発言・停止・終了・期限で失効」を表示する。private_requiredは「公開できない情報があるため補足確認が必要」等の理由＋「本人限定で確認」＋「拒否」。一覧入口は `/mcp` のみ。

秘密の値をDiscord custom_idへ埋め込まない。ProxyトークンはGatewayのDBに保持し、custom_idにはローカル参照IDを使う。Discord mention・リンクプレビュー・Markdownの意味変更を防ぐエスケープとallowed_mentions無効化はGatewayの責任。値を翻訳／要約／省略して承認判断に使わない。

## 9. 上限・エラー

公開display内のすべての文字列（固定disclosure/provenanceを除く）の合計はUTF-16 code unitsで1400以内、fields最大8。制御文字・双方向表示制御文字を許すrendererは提供しない。Proxy応答全体は非圧縮UTF-8 JSONで32768 bytes以内。Gatewayのボタン文言・案内・エスケープ後にDiscord制限を超える場合は、文字を切り落とさず本人限定補足へ戻す。公開添付ファイルや複数投稿に分けて承認対象を曖昧にしない。API本文・IDの既存上限も適用し、小さい方を優先する。

| HTTP | code | 動作 |
| --- | --- | --- |
| 400 | invalid_approval_presentation | フィールドの修正。許可未送信 |
| 409 | presentation_context_mismatch | 対象会話・Response宣言を照合。別投稿先へ流用しない |
| 409 | presentation_conflict | 表示版・内容・方針が変化。旧押下を破棄して再表示 |
| 409 | presentation_expired | 停止・世代・期限・再起動等で失効。状態照会へ |
| 422 | inline_approval_unavailable | 公開情報で承認不可。取得可能なら本人限定補足へ |
| 503 | inline_approval_disabled | capability照会、0.3の画面を使用 |
| 503 | presentation_store_unavailable | 保存不能、許可しない。既存キーの結果照会を優先 |

共通エラーbodyは0.3と同じ。新規コードのretry.actionはnone。GET時、Responseが未宣言なら409 presentation_context_mismatch。対応付け不能等の正常な非提供は200 unavailable、closedも200 unavailable。既存404、認証、復元保留、workspace失効、revision／scope競合、turn_grant_ineligible、operation unknownは継承する。

## 10. 必須受入（実装後）

| ID | 合格条件 |
| --- | --- |
| I01 | 実上流callに対する公開操作・対象が一致。偽のMCP説明・未知引数・同名別定義でinlineにしない |
| I02 | 5.2の認証候補・資格形式・userinfo・署名URL・コード・秘密入力・多重encodingを使ったfixtureで、対象値が公開カード・DB・ログに出ない。通常の検索語・URLは元値を欠落なく表示。未識別の秘密の完全検出を保証した扱いにしない |
| I03 | 初回同一カードで内容と選択肢を提示。通常inlineは詳細ボタン不要。対象外は2択。上流常時許可を変更しない |
| I04 | audience・本人・Run・message ID・表示版・scope・世代の1要素違いで送信0件 |
| I05 | 同じinteraction/revisionでも表示内容・公開方針変更で旧トークンを拒否。表示と送信意思保存の競合を制御 |
| I06 | private_required／unavailable／省略・未知reason・長文・エスケープ後超過で公開ボタンから許可しない |
| I07 | Discord送信前／送信中／結果不明／DB確定前の押下、二重押下、Gateway再起動で未確認表示への送信なし |
| I08 | Proxy再起動・復元・停止・Steer・イベント欠落・時計後退で旧表示と許可が復活しない |
| I09 | 返信結果不明を同一キーで照会。失効後の同一キー再照会も元結果を返し、再実行しない |
| I10 | 新旧Gateway／Proxyの組合せで非対応の新フィールドを推測使用しない。0.3の秘密保持とv1互換を維持 |
| I11 | 実Discordで本人が検索語のあるbrowser_findとURLのあるbrowser_navigateを最初のカードから承認。単発・ターン・拒否・補足・取消も操作。機密fixtureは実秘密を使わず例外を確認。通常検索を全件補足へ戻す実装は不合格 |

## 11. Gateway要求との対応・レビュー判断

| 要求 | 本案の回答 |
| --- | --- |
| 公開と本人限定の分離 | 新presentation API。既存argumentsはrequester_onlyのまま |
| 操作・対象・制限／秘密保持 | 評価済みrenderer、公開可能性と情報充足を別判定、省略時は補足 |
| 表示と実行の固定 | callのscope fingerprintに加え、不透明presentation tokenをProxyでも検証 |
| 適格性と公開可能性の独立 | actionsを分離。evaluateの個別確認維持 |
| 例外フロー | private_required／unavailableと理由・ボタン・長文処理 |
| 返信・失効・冪等性 | 既存reply拡張。照合と送信意思保存を同じ境界で実施 |

### 公開方針の決定記録

利用者から「操作対象を元の会話に表示する（おすすめ）」を選択し「おすすめで決定」との回答を受領した。検索語・アクセス先URLの公開方針はこれで確定。初稿の「自由文の公開方針待ち」「browser_findは一律補足」は本版で置き換える。Proxyが元の会話の閲覧者へ操作対象を表示する範囲は第5節のとおりで、認証情報・秘密入力・任意の実行コードの公開へ拡大しない。

Gatewayへは本版のフィールド、renderer、除外規則、同一カード配信・返信照合・上限を照合してもらう。文書の公開方針は確定済みだが、0.4の接続レビュー完了・実装・受入・配備とは別。再度同じ公開方針の承認を求めることなく、接続上の指摘へ対応する。

実装開始条件は本版と要件・設計の整合、Gateway接続レビュー完了。現行0.3の常駐設定は維持し、新機能が利用できると案内しない。

### Gateway R-01への回答（2026-09-16）

Gatewayの `docs/mcp-inline-approval-api-review.ja.md` を確認した。第2〜4・6〜10節の境界に異論なしとの回答を受領。R-01の対応は以下のとおり。

| 指摘 | 反映先 |
| --- | --- |
| 利用者の公開方針の反映 | 第3・5・11節。公開方針の保留を解消 |
| 検索語／regex、URLのrenderer ID・引数形式・fields | 第4節の検索応答例、第5.1節と公開fields表。登録済みツールの公開定義を確認 |
| 秘密混在・未知引数・コード・長文 | 第5.2節、private_required、I02/I06。対象値全体を公開せず補足へ |
| 常時許可のbrowser_findへ確認を新設しない | 第1・5節。renderer対応と上流承認方針を分離 |
| 単発／ターン／補足actionsと受入 | 第5節のactions一覧、第10節I03/I11。通常検索・URLで最初のカードから許可できることを必須化 |

これはProxy側の文書対応記録。Gatewayによる本改訂版の接続レビュー完了を代わりに宣言するものではない。日英の実利用マニュアルはGateway側の記録に従い実装・試験後に更新し、現在は未提供の契約案としてリンクする。

### 実装開始記録

Gatewayの接続レビュー完了記録と、利用者の実装開始指示を確認。0.4を基準に実装へ進む。上記の再レビュー待ち記述は提示時点の履歴。具体アダプタは信頼された運用者のMCP接続と上流catalogの引数schemaを照合し、その接続・定義を設定世代へ拘束する。リモート内部実装の無通知変更の検知保証は0.3同様に範囲外。catalog取得失敗・未知schemaでは補足へ戻す。
