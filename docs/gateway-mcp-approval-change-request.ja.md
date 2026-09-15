# Gateway改定依頼：MCP承認・対話と停止理由の表示

2026-09-15。ProxyからGatewayへの実装依頼。Gateway実装済みという前提ではない。既存の [対話API契約](interaction-api.ja.md) を使い、基本契約2.0・OpenAI互換v1は変更しない。

## 実障害と原因

22:29／22:30のDiscord実行で、lightpandaを使う依頼が `unsupported_interaction` により停止した。Responseは `resp_210b9a79-3ad7-4ec0-921a-05ec1b9b89eb`、`resp_88aba9de-6731-4bde-b9e8-7ce2e82839ba`。どちらも `interaction_capabilities: []`、`execution_status: interrupted`。Proxyは未対応の上流対話へJSON-RPC -32601を返した。Gatewayは理由を区別せず「作業を中断しました」と表示した。

利用者が拒否した証拠はない。上流モデルが出した `user rejected MCP tool call` を利用者操作の事実として表示・説明しない。MCPが見えることと、Discordで承認して実行できることは別の受入条件である。

## 必須対応

1. v2実行要求の `interaction_capabilities` に、実装・検証した種類だけを宣言する。今回の最低限は `mcp_form`。宣言だけ追加せず、以下の取得・表示・回答・失効処理を同時に実装する。通常の `metadata["codex.approval_capability"]: "interactive"` ではMCP対話対応の代用にならない。
2. 実行中は `GET /v2/codex/responses/{response_id}/interactions` を定期取得する。イベント通知は補助とし、通信復帰・再起動後もGETで照合する。
3. ツール承認は、`kind: mcp_form`、`request._meta.codex_approval_kind: mcp_tool_call`、空propertiesのフォームとして届く場合がある。serverNameとProxyが返す承認文面を省略せず表示し、「今回許可」「拒否」を提供する。文面からコマンドやツール引数を推測して作らない。
4. 許可は `POST /v2/codex/interactions/{id}/reply` へ `{"expected_revision":<取得値>,"response":{"action":"accept","content":{}}}`。拒否は `action: decline, content: null`。instance／復元世代・Bearer・永続化したIdempotency-Keyを送る。許可の既定選択、自動送信、将来の全ツールへの包括許可はしない。
5. `mcp_form` はツール実行前の承認だけでなく、ツール内部からの入力フォームも含む。契約のflat-primitives-v1（string/integer/number/boolean、required/enum等）全体を扱う。空フォームだけ実装して `mcp_form` 全対応を宣言しない。入力を処理できない場合は明確な理由を表示して停止・辞退する。
6. ツール実行承認の後に別のフォームが来る場合を扱う。Responseあたり1回の承認という前提にしない。`mcp_url`・`permissions`・`user_input` はそれぞれのUIと検証が完成した段階で宣言し、通常承認へ変換しない。

## 操作の認可・復旧

- Discord利用者と対象依頼の操作権限をGatewayが検証する。Proxyの共有APIキーをDiscordへ渡さない。
- UIをResponse・会話・interaction ID・revisionへ結び付ける。二重クリック、別利用者、別会話、期限切れ、停止済み、古い復元世代を拒否する。
- 回答送信前に操作キーと対象を永続化。HTTP切断時は元キーのoperationとinteractionを照合し、新規キーで再送しない。Proxyが `unknown` を返した回答は再実行しない。
- `submitted` は書込み完了であり、MCP実行成功ではない。表示を承認送信済みに更新し、最終結果を別途確認する。
- resolved/expired/cancelled/unknown、Turn終了時は古いボタンを無効化する。Bot再起動で同じ承認への重複UIを増やさず、可能なら元メッセージを更新する。
- `/stop` は既存のProxy停止契約を使う。未解決フォームへ勝手にacceptを返さない。

## エラー表示の修正

`src/proxy_v2.rs` の `execution_status: interrupted -> Cancelled` の対応付けだけで終了理由を決めず、Proxyの `error.code` と保存済みの利用者停止操作を保持する。`unsupported_interaction` は「必要な承認・入力画面に未対応のため実行できませんでした。利用者による拒否ではありません」等と表示する。利用者拒否・利用者停止・承認期限切れ・上流障害・結果不明も区別する。理由を取得できない場合は不明とし、利用者の行為と断定しない。AIの最終文面だけを診断根拠にしない。

## 責務分担と設定更新

Proxyは共通MCP設定を新規／継続依頼の開始前に確認し、変更時にApp Serverを再読込する。会話ID、ワーク、履歴を維持する。Gatewayがチャットを作り直したり、Codexプロセスを再起動したりする必要はない。GatewayはUI・操作認可・配信、Proxyは実行・対話状態・承認応答の照合を担当する。

## 結合受入（完了条件）

- 既存DiscordチャットでYahoo! JAPANを開く依頼 → lightpanda承認画面 → 許可 → 実際のMCP成功 → 見出し表示。モデル文面だけでなくProxyのTurn内のMCP実行結果も照合する。受入用の実Discord操作は利用者と実施する。
- 同じ依頼の拒否：アクセスが実行されず、拒否として表示される。
- テストMCPで、ツール承認→必須入力フォーム→入力→正常完了の二段階。
- 二重クリック、無権限利用者、停止との競合、期限切れ、古いボタン、Bot再起動、回答送信後の通信切断、結果不明で重複実行・重複回答がない。
- 未宣言クライアントは引き続き安全に停止し、理由を正しく表示する。
- MCP設定の変更前後で同じProxy会話ID／Thread IDを維持し、新規登録したツールが次の依頼で利用できる。

Proxyの読み取り専用 `session_list` 成功だけでは、このGateway受入を完了にしない。
