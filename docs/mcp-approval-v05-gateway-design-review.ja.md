# Gateway 0.5内部詳細設計 — Proxy側照合結果

2026-09-17。初回対象はGatewayの内部詳細設計1.0、要件2.6、システム設計1.6。**改訂1.1の再照合によりGD-01〜03は解消。今回のGateway内部設計の接続照合はGo。以下の指摘は初回レビューの履歴として残す。** 製品コード・常駐環境は変更していない。Proxy全ツール詳細設計の未完了も別に残る。

## 対象文書

- [Gateway内部詳細設計1.0](../../codex-hoshikage-gateway/docs/mcp-approval-v05-gateway-design.ja.md)
- [Gateway要件2.6](../../codex-hoshikage-gateway/docs/requirements.ja.md)
- [Gatewayシステム設計1.6](../../codex-hoshikage-gateway/docs/system-design.ja.md)
- [合意済みProxy契約0.5](mcp-approval-full-fix-api.ja.md)

## GD-01：長文によるprivate_requiredを機密情報の例外から分けて受理する

対象：Gateway詳細設計第3節「契約上の例外だけ」、第3.1節の公開投稿分割、要件UI-04「本人向け情報だけ補足表示へ進む」。Proxy契約第4節のdisplay_too_large、第5節。

Proxyは機密性がない内容でも、公開用の1ページ上限に収まらなければstate=private_required、reason=display_too_largeを返す。Gatewayの記述は本人向け情報だけに入口を限定しており、この正常応答の経路が抜けている。長文が未対応扱いになる、または許可できない状態に止まるおそれがある。

次の二つを明示して分ける。

1. **Proxyの公開1ページに収まる内容**：GatewayがDiscordの1900単位ごとの複数投稿へ分ける。同一公開表示版の全投稿を確認し、通常の承認導線を維持する。1投稿に収まらないという理由だけで本人向けへ移さない。
2. **Proxyがdisplay_too_largeでprivate_requiredを返す内容**：「内容が長いため、自分だけに表示して順に確認」と案内し、requesterの全ページを確認する。機密情報だと説明しない。本人向けの内容をGatewayが独自判断で公開へ戻さない。

G05-02/03へ、非機密の公開可能な複数投稿と、公開ページ上限超過でのrequester複数ページを別々に追加する。後者でも途中拒否可能、全ページ配信前の許可不可を確認する。

## GD-02：拒否を許可の表示検証経路から分ける

対象：Gateway詳細設計第6節の手順2〜4。Proxy契約第3節（null条件）、第5・7節（全表示不要の拒否）、第12節（更新待機中も拒否可能）。

手順4にはdeclineはページ表示不要とあるが、その前の共通手順2でscope・fingerprint・全ページtokenを検証すると読める。unavailableではこれらがnullであり、途中ページや取得失敗で拒否まで遮断しかねない。private/next/prev/recheckにも許可専用条件を共通適用しないよう、action別に分岐を明記する。

- 共通：本人・Guild・実会話・interactionとの対応、既存decision、現在のinteraction状態を検証する。
- once/turn：現在のpresentation・全scope・版・全ページtoken・全配信確定を検証する。
- decline：現在のinteractionとrevisionを取得し、拒否可能なpendingであれば従来の拒否bodyを送る。presentationがunavailable、scope=null、ページ未取得でも拒否できる。既に解決済み・結果不明・別のdecisionが確定済みなら、その実状態を返し新たに送信しない。
- private/next/prev/recheck：本人認可・表示版と該当操作の条件を確認して取得／表示だけ行う。全ページの表示成功をナビゲーションの前提にしない。

ネットワーク自体が不通なら拒否成功と表示せず、送信状態を扱う。別の制御枠があるだけでは、検証条件による拒否遮断の解決にはならない。

G05-04/06/10へcatalog_loading、scope=null、最初のページだけ配信済み、後続ページ取得失敗での拒否を追加。認可が正しく現在pendingなら、全ページtokenなしで拒否経路へ進み、上流へ許可を送信しないことを確認する。

## GD-03：返信JSONの項目名を合意契約と一致させる

対象：Gateway詳細設計第6節手順5。Proxy契約第7節。

手順5はscope_fingerprint／presentation_fingerprintを送信すると読めるが、要求bodyの正式名は**expected_scope_fingerprint／expected_presentation_fingerprint**。DB列名や応答フィールド名とは区別する。これを誤った名前のまま実装すると正常な承認が受理されない。省略記法の意図であっても、内部設計に完全なbody例を置く。

単発の許可：

```json
{
  "expected_revision": 1,
  "expected_scope_fingerprint": "opaque-scope",
  "approval_view": "source_conversation",
  "expected_presentation_fingerprint": "opaque-presentation",
  "expected_page_tokens": ["opaque-page"],
  "response": {"action": "accept", "content": {}}
}
```

依頼中許可は上記へgrant_scope=turn_toolだけを追加。requesterならapproval_viewをrequesterとし、その表示版のtokenを使う。Idempotency-Keyは従来どおりHTTP header。拒否のbodyは別経路であり、これらの許可用フィールドを必須にしない。

G05-06へ送信JSONの厳密照合を追加する。内部DB列からの再構成・再起動後の照会でも、同じoperation keyに異なるbodyを結び付けない。

## 利用者向け文言の小修正

詳細設計第3・8節の「本人限定で確認」は、利用者と合意した**「自分だけに表示して確認」**に統一する。「同じ会話の他の参加者には見えない」「開いただけでは実行しない」を説明する。これは新機能の追加ではなく、既に合意した導線を分かる言葉で示す修正。

## 整合している点

- Response別profile固定、旧版への暗黙変更禁止、全scope・新世代の保存。
- ProxyのページとDiscord投稿を分け、全配信確定・本文非永続化・鍵付き照合を用いる設計。
- 正常catalog更新中はProxyの再評価を待ち、Gatewayが許可を代送しない設計。
- 公開投稿の再照合と、本人向けの明示的な再表示を区別した再起動復旧。
- 未確定配信・返信の再送抑止、Stop等の別制御枠、非MCP承認の完全表示。

GD-01〜03は1.1で文書修正を確認し、今回のGateway内部設計照合を完了した。コード不具合を確認したレビューではない。双方の内部詳細設計完成・実装・合成試験・実Proxy・実Discord受入をそれぞれ区別する。

## 改訂1.1の再照合結果

- GD-01：第3・3.1節に非機密のdisplay_too_largeと、公開1ページ内のDiscord分割の区別を確認。G05-02/03にも反映。解消。
- GD-02：第6.1〜6.4節で拒否と許可を分離。取得失敗・scope=null・途中ページでも現在pendingなら拒否可能。decisionの許可用フィールドのnull条件と受入も追記。解消。
- GD-03：第6.3節の完全JSONをProxy契約第7節と照合。expected_scope_fingerprint／expected_presentation_fingerprintを使用し、単発ではgrant_scope省略。拒否bodyを別例へ分離。解消。
- ボタンは「自分だけに表示して確認」へ統一済み。

Gatewayへの追加差し戻しなし。Proxy側の全ツール詳細設計・部分置換の根拠取得と版対応は未完了。Gatewayの修正完了を、Proxy内部詳細設計や双方の製品実装Goへ読み替えない。新たな利用者の方針判断を求めるものではなく、Proxy側の残作業である。
