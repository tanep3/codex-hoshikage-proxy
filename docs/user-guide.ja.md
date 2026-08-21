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
| `defaults.model` | リクエストにmodelがない場合のモデル |
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
`inProgress`、`completed`、`interrupted`、`failed`の状態、取得できた失敗理由、Proxyが最後に受信したイベント時刻を返します。
これにより、長時間処理中なのか、イベントストリームが止まったのかを切り分けられます。

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

MVPで対応する主なフィールドは `model`、`input`、`previous_response_id`、`stream`、`metadata` とChatGPT専用の `reasoning` です。
標準イベントとしてレスポンス作成、テキスト差分、ツール呼び出し／結果、usage、完了、キャンセル、エラーを扱います。

返されたレスポンスIDで会話を継続できます。

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "previous_response_id": "resp_123",
  "input": "Continue."
}
```

Proxy再起動後もCodex側Threadが利用可能な場合だけ継続できます。利用不能なら `thread_not_found` となり、会話文から擬似復元はしません。

## Chat Completions API

OpenWebUIはこのエンドポイントを使用します。

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"ollama/gemma4:e4b","messages":[{"role":"user","content":"Say OK."}],"stream":true}' \
  http://127.0.0.1:4040/v1/chat/completions
```

これはOpenAI API全体ではなく、互換サブセットです。MVPではテキストmessages、`model`、`stream`、`metadata` と、Proxyのツール／承認フローを中心に対応します。
マルチモーダル入力、`tool_choice`など高度な項目を使う前に、モデルの能力と対応範囲を確認してください。

## エラーと承認

- `401 invalid_api_key`: クライアント認証失敗。
- `404 model_not_found`: 公開モデルIDが未登録。
- `409 approval_required`: 承認機能のないクライアントで承認要求が発生。ProxyはCodex側を拒否／キャンセルしてTurnを解放し、タイムアウト待ちはしません。
- `409 thread_not_found`: 継続対象のResponses Threadが利用不能。
- `400 unsupported_parameter`: 対応しないプロバイダ固有オプション。
- `turn_failed`等: 利用可能ならCodexの失敗詳細を含みます。

承認は承認、拒否、キャンセル、期限切れのいずれかまでPendingです。MVPでは承認待ち中もTurn全体のProvider permitを保持します。クライアント切断時はTurnをキャンセルします。
`approval.auto_approve_workspace = true` の場合、Codexが指定ワークスペース内の操作として報告した承認要求は自動承認します。
信頼したローカル作業向けの設定であり、信頼できない利用者へProxyを公開してよいという意味ではありません。許可するcwdは狭く保ってください。
ワークスペース外の操作は、これまでどおり対話承認の対象です。

Sandbox設定とProxyの自動承認設定は別物です。`codex.sandbox.writable_roots` はProxyが生成するCodex設定へ反映されるため、実在する絶対パスを指定してください。
ワークスペース内判定では、Codexが構造化して渡す `cwd`、`path`、`file_path`、`filePath`、`target_path`、`targetPath` だけを使用します。コマンド文字列にワークスペースのパスが含まれているだけでは、自動承認しません。
外部ネットワークを必要とする信頼済みのローカルSkillを使う場合だけ、`network_access = true` にしてください。

## セキュリティと運用

- リモート公開が不要ならloopbackで待受。
- リモート公開時はAPI Keyを使い、TLSはリバースプロキシへ委譲。
- 特別な理由がない限りCORSは無効のまま。
- 作業ディレクトリの許可ルートは狭く設定し、事前に存在させる。
- Event Journalはメタデータ中心で、ローテーションと保持期間を運用で設定。出力やファイル内容はサイズ制限／redaction対象。
- 信頼できない利用者へCodex実行を公開しない。クライアントAPI Keyは承認やファイルシステム制御の代わりにはなりません。
