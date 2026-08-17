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
| `security.allowed_cwds` | Codexが使える実在する正規化済みディレクトリのルート |
| `security.api_key` / `api_key_env` | クライアント認証。非loopbackでは必須 |
| `defaults.model` | リクエストにmodelがない場合のモデル |
| `approval.timeout_seconds` | 承認の有効期限 |
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

## セキュリティと運用

- リモート公開が不要ならloopbackで待受。
- リモート公開時はAPI Keyを使い、TLSはリバースプロキシへ委譲。
- 特別な理由がない限りCORSは無効のまま。
- 作業ディレクトリの許可ルートは狭く設定し、事前に存在させる。
- Event Journalはメタデータ中心で、ローテーションと保持期間を運用で設定。出力やファイル内容はサイズ制限／redaction対象。
- 信頼できない利用者へCodex実行を公開しない。クライアントAPI Keyは承認やファイルシステム制御の代わりにはなりません。
