# ユーザー／APIガイド

**日本語** | [English](user-guide.md)

## 環境設定

標準の設定ファイルは次の場所です。

```text
~/.config/codex-hoshikage-proxy/config.toml
```

`CODEX_HOSHIKAGE_PROXY_CONFIG` で別のファイルを指定できます。設定は宣言的に管理し、Proxy専用のCodex `config.toml` は
設定から再生成します。生成されたCodex設定を手編集しないでください。

| 設定 | 意味 |
| --- | --- |
| `server.host`, `server.port` | 待受アドレス |
| `server.default_cwd` | 実在するデフォルト作業ディレクトリ |
| `server.turn_idle_timeout_seconds` | 1つのTurnでCodex App Serverからイベントが届かない最大時間。デフォルトは`600`秒。タスク全体の制限時間ではありません |
| `server.turn_stall_detection_seconds` | イベントが届かないときに、異常な停止の可能性をCodexへ確認するまでの時間。デフォルトは`180`秒 |
| `server.turn_stall_confirmation_count` | `turn_stalled`と判定する連続無進捗確認回数。デフォルトは`3`回 |
| `server.turn_heartbeat_seconds` | Turn実行中にSSE heartbeatを送る間隔。デフォルトは`30`秒 |
| `security.allowed_cwds` | Codexが使える実在する正規化済みディレクトリのルート |
| `security.api_key` / `api_key_env` | クライアント認証。非loopbackでは必須 |
| `defaults.model` | 新規会話／Chatでmodelを省略した場合のモデル。Responses継続ではThreadの最新選択を継承 |
| `approval.timeout_seconds` | 承認の有効期限 |
| `approval.auto_approve_workspace` | 指定ワークスペース内のCodex操作を自動承認するか。デフォルトは `true` |
| `codex.sandbox.mode` | 新しいThreadで使うCodexのsandboxモード。デフォルトは `workspace-write` |
| `codex.sandbox.writable_roots` | workspace-writeで追加して書き込み可能にする、実在する絶対パスの一覧 |
| `codex.sandbox.network_access` | workspace-write中のコマンドから外部ネットワークへ接続するか。デフォルトは `false` |
| プロバイダの `enabled` | プロバイダの有効化 |
| プロバイダの `max_concurrent_turns` | 同時実行数 |
| `models."provider/model"` | 公開IDと上流モデルの対応 |

公開モデルIDではプロバイダとモデルを一緒に選びます。推論レベルは独立した指定ですが、現在は `chatgpt/...` のみ受け付けます。
値はCodexのモデル設定に従います。他プロバイダでは推論指定を省略し、プロバイダ側のデフォルトを使います。

## 統合モデル一覧

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  http://127.0.0.1:4040/v1/models
```

OpenAI互換の一覧で、次のようなIDが返ります。

```text
chatgpt/gpt-5.6-luna
hoshikage/unsloth-gemma4-12b-qat-thinking-off
ollama/gemma4:e4b
```

## Turn状態の診断

実行中のCodex Turnは、次のAPIで確認できます。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  http://127.0.0.1:4040/v1/codex/turns/{turn_id}/status
```

ProxyはCodex App Serverの標準`thread/read`メソッドへ`includeTurns=true`を付けて問い合わせます。
`status`は`inProgress`、`completed`、`interrupted`、`failed`、または照会できない場合の`unknown`です。
保存済みの観測値は`last_observed_status`と`last_observed_at_ms`として別に返します。
互換フィールド`last_event_at_ms`もこの状態観測時刻であり、進捗イベントの最終受信時刻ではありません。
`unknown`を完了や失敗として扱わないでください。

`server.turn_idle_timeout_seconds`は、Codex App Serverからイベントが届かない時間の上限です。
タスク全体の実行時間ではありません。期限を超えるとProxyはCodex Turnへinterruptを送り、
`runtime_idle_timeout`を返します。デフォルトは600秒です。

それより前に`server.turn_stall_detection_seconds`を超えると、Proxyは`thread/read`でCodexの状態を確認します。
承認待ちは継続しますが、Codexが実行中のまま進捗イベントを返さない場合は、ProxyがTurnをinterruptし、
`turn_stalled`を返します。

Hoshikageは通常一覧と詳細な能力一覧を組み合わせます。詳細情報の `tools: false` のモデルはCodexエージェント実行に必要な
ツール呼び出しに対応しないため、動的一覧へ公開しません。モデル名から推測はしません。
HoshikageとOllamaのカタログ取得は5秒でタイムアウトします。どちらかのサービスが停止中でもProxyは起動を継続し、そのProviderの動的モデルだけを一覧から除外します。
カタログは `/v1/models` の要求時とモデル使用時に毎回更新するため、Providerが復旧すれば次の要求で自動的に一覧へ復帰します。

## Responses API

ストリーミングなし:

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"Say OK."}' \
  http://127.0.0.1:4040/v1/responses
```

ストリーミング:

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","input":"Say OK.","stream":true}' \
  http://127.0.0.1:4040/v1/responses
```

MVPで対応する主なフィールドは `model`、`input`、`previous_response_id`、`stream`、`metadata`、`text.format` とChatGPT専用の `reasoning` です。
現在の標準出力はレスポンス作成、テキスト差分、完了・失敗が中心です。ツール呼び出し／結果・usageの完全なOpenAI形式変換は未完成です。承認はCodex拡張APIで扱います。

返されたレスポンスIDで会話を継続できます。

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "previous_response_id": "resp_123",
  "input": "Continue."
}
```

Proxy再起動後は`thread/resume`で保存済みThreadを再読み込みし、利用可能な場合に継続できます。利用不能なら `thread_not_found` となり、会話文から擬似復元はしません。

継続できるのは成功したResponseだけです。同一Provider内なら`model`を指定して、履歴を保ったまま次のTurnからモデルを変更できます。
継続時の`model`省略は、参照した古いResponseのモデルではなく、そのThreadで最後に開始が受理されたモデルを継承します。
Providerをまたぐ変更は409、同一Threadの実行競合も409です。付属OpenWebUI Pipeは引き続きモデル変更時に新規Threadを作ります。

## Chat Completions API

通常のOpenAI互換Chatクライアント向けです。付属のOpenWebUI PipeはResponses APIを使用します。

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"ollama/gemma4:e4b","messages":[{"role":"user","content":"Say OK."}],"stream":true}' \
  http://127.0.0.1:4040/v1/chat/completions
```

これはOpenAI API全体ではなく、互換サブセットです。テキスト／画像messages、`model`、`stream`、`metadata`、`response_format`、ChatGPT専用の`reasoning_effort`と、Proxyの承認フローに対応します。クライアント定義のtool callingとの完全互換を保証するものではありません。
マルチモーダル入力、`tool_choice`など高度な項目を使う前に、モデルの能力と対応範囲を確認してください。

## エラーと承認

- `401 invalid_api_key`: クライアント認証失敗。
- `404 model_not_found`: 公開モデルIDが未登録。
- `409 approval_required`: 承認機能のないクライアントで承認要求が発生。ProxyはCodex側を拒否／キャンセルしてTurnを解放し、タイムアウト待ちはしません。
- `404 thread_not_found`: 継続対象のResponses Threadが利用不能。
- `400 unsupported_parameter`: 対応しないプロバイダ固有オプション。
- `turn_failed`等: 利用可能ならCodexの失敗詳細を含みます。

承認は承認、拒否、キャンセル、期限切れのいずれかまでPendingです。MVPでは承認待ち中もTurn全体のProvider permitを保持します。クライアント切断時はTurnをキャンセルします。
`approval.auto_approve_workspace = true` の場合、Codexが指定ワークスペース内の操作として報告した承認要求は自動承認します。
信頼したローカル作業向けの設定であり、信頼できない利用者へProxyを公開してよいという意味ではありません。許可するcwdは狭く保ってください。
ワークスペース外の操作は、これまでどおり対話承認の対象です。複数のパスが報告された場合はすべてを確認し、1つでも外部のパスがある場合や対象パスが不明な場合は自動承認しません。

Sandbox設定とProxyの自動承認設定は別物です。`codex.sandbox.writable_roots` はProxyが生成するCodex設定へ反映されるため、実在する絶対パスを指定してください。
ワークスペース内判定では、Codexが構造化して渡す `cwd`、`path`、`file_path`、`filePath`、`target_path`、`targetPath`、`grantRoot`、`paths`、`fileChanges`、`commandActions`内のパスと、`item/started`等で通知された変更対象を使用します。コマンド文字列にワークスペースのパスが含まれているだけでは、自動承認しません。
外部ネットワークを必要とする信頼済みのローカルSkillを使う場合だけ、`network_access = true` にしてください。

## セキュリティと運用

- リモート公開が不要ならloopbackで待受。
- リモート公開時はAPI Keyを使い、TLSはリバースプロキシへ委譲。
- 特別な理由がない限りCORSは無効のまま。
- 作業ディレクトリの許可ルートは狭く設定し、事前に存在させる。
- Event Journalと実行台帳に自動ローテーション・期限削除はありません。ディスク使用量を監視してください。
- `state/responses/mappings.jsonl`と`executions.jsonl`は会話継続・重複実行防止に必要です。更新や復旧時に削除せず、同じ状態ディレクトリを複数Proxyで共有しないでください。
- 実行台帳はメタデータを保存し、プロンプトや最終出力の再取得用ストアではありません。
- 信頼できない利用者へCodex実行を公開しない。クライアントAPI Keyは承認やファイルシステム制御の代わりにはなりません。

## 画像入力と構造化出力

Responsesの`input`は文字列、テキスト／画像の配列、または`role`と`content`を持つメッセージ配列を受け付けます。
Chat Completionsの`messages[].content`も文字列とテキスト／画像の配列に対応します。
メッセージのroleは従来どおり`[user]`などのラベル付きテキストに変換します。App Serverに独立したsystemメッセージとして渡す方式ではありません。

Responsesの例:

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "input": [{"role": "user", "content": [
    {"type": "input_text", "text": "画像の内容を説明して"},
    {"type": "input_image", "image_url": "https://example.com/image.png", "detail": "high"}
  ]}],
  "text": {"format": {
    "type": "json_schema",
    "name": "description",
    "strict": true,
    "schema": {
      "type": "object",
      "properties": {"description": {"type": "string"}},
      "required": ["description"],
      "additionalProperties": false
    }
  }}
}
```

Chat Completionsでは画像を`{"type":"image_url","image_url":{"url":"https://example.com/image.png","detail":"high"}}`で指定します。
出力形式は`response_format: {"type":"json_schema","json_schema":{"name":"description","schema":{...},"strict":true}}`で指定します。
ChatGPTプロバイダでは`reasoning_effort: "high"`も指定でき、そのモデルが公開する対応値に対して検証します。

- 画像はHTTP(S) URLまたは`data:image/...` URLをCodexへ渡します。`file://`、サーバー上のローカル画像パス、Files APIのIDは受け付けません。
- `detail`は`auto`、`low`、`high`、`original`に対応します。画像対応状況と取得可否は使用するモデルとCodex環境に依存します。
- JSON Schemaは`turn/start.outputSchema`へ転送します。最終回答はJSON文字列として返し、Proxyによる追加のSchema検証は行いません。`name`や`strict`はCodexへの独立したオプションにはなりません。
- `text`と`json_schema`以外の出力形式は400エラーになります。
- ChatGPTのモデル一覧は`nextCursor`がなくなるまで取得します。循環するカーソル、100ページ超過、取得全体の5秒タイムアウトはカタログ取得失敗として扱います。

仕様との対応状況と残る未対応機能は[App Server対応表](app-server-coverage.md)を参照してください。

実接続テストではCodex 0.153.4＋gpt-5.6-lunaの`detail=low`で色の誤認が再現しました。
同じ画像は`detail=high`で正しく認識しています。詳細は[実接続テスト結果](live-codex-validation.md)を参照してください。

## 汎用の実行制御

開始時の識別子、Idempotency-Key、状態照会、Steer、中断、承認抑制、会話モデル変更の契約は[制御API v1](control-api.ja.md)を参照してください。同一Provider内のモデル変更は次のTurnに反映し、会話履歴を保持します。

Responsesに`Idempotency-Key`を付けると要求IDで照会できます。同じキー・同じ本文の再送は実行記録を返し、出力やSSEを再配信しません。
監視SSEの再接続はsnapshot方式で、過去イベントの差分再生はありません。生成接続の切断はTurn中断を伴いますが、監視接続の切断は中断しません。
制御APIは同じAPIキーを共有する運用者向けで、利用者ごとの分離はありません。

## 未対応の対話・ツール指定

Codexが質問、MCP追加確認、権限専用承認を要求しても、Proxyが回答や許可を代行して捏造することはない。対応を宣言したv2クライアントには[対話中継API](interaction-api.ja.md)を提供する。未宣言・未対応の場合は `unsupported_interaction` を返して対象Turnの停止を要求する。停止完了はTurn状態で確認し、エラーだけを根拠にAIを再実行しない。通常のコマンド／ファイル承認は既存APIを使う。ネットワーク承認・追加権限・ルール追加の要求はワーク内自動承認の対象外とする。

OpenAI互換APIでのクライアント定義 `tools` / `functions`、`tool_choice` / `function_call`、`parallel_tool_calls` とツール呼出し履歴の中継は未対応。動作を変える指定は受付前に400 `unsupported_parameter`で拒否する。空のツール一覧、`null`、呼出し選択の `none`、並列指定の `false` はツール利用を要求しない指定として受理する。Codex内部のツール実行とv2成果物登録ツールは別経路で継続する。配備状況と検証範囲は[追加受入記録](proxy-hardening-2026-09-13.ja.md)を参照。
