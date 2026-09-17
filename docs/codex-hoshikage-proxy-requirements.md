# Codex Hoshikage Proxy 要件定義書

## 1. 文書情報

| 項目 | 内容 |
|---|---|
| 文書名 | Codex Hoshikage Proxy 要件定義書 |
| プロジェクト名 | Codex Hoshikage Proxy |
| 文書種別 | 要件定義書 |
| 対象フェーズ | 製品版（v1互換API・v2 Gateway契約） |
| 状態 | Draft |
| 想定公開先 | GitHub |
| ライセンス | MIT License を想定 |
| 設定ルート | `~/.config/codex-hoshikage-proxy` |

本書は製品としての動作・障害復旧・責務境界を定める。Draft表記は未検証項目を含むことを示し、品質要件を縮小する意味ではない。現時点で合意済みの項目は各節の要件および「現時点での決定事項」に記載する。

### v2製品契約の実装方針

[合意済み契約0.2](workspace-artifact-api-v2.ja.md)をワーク・成果物・回答復旧の規範とする。
SQLiteによる会話・操作・占有・停止意思・保存物・リース・監査のtransaction、私有ファイルの同期公開、
会話IDでの永続実行、HTTP切断と明示停止の分離を実装する。Discord認可・キュー・配信結果はGatewayが所有する。
以下の旧Draft記述と競合する場合はv2契約を優先する。既存v1の提供範囲は維持し、v2の実装・受入状況は
[v2実装記録](v2-implementation-status.ja.md)で別管理する。実装構造は[v2実装設計](v2-system-design.ja.md)、管理操作は[v2運用手順](v2-operations.ja.md)を参照する。未検証機能を利用可能と表示しない。

### 実装・検証状況（2026-09-13更新）

本書には未実装の要求・設計を含むため、全体の状態はDraftのままとする。現在の提供範囲は
[App Server対応表](app-server-coverage.md)、実機で確認した範囲は[実接続テスト結果](live-codex-validation.md)を参照する。
Codex 0.153.4＋gpt-5.6-lunaで主要APIと切断・承認キャンセル／期限切れ・異常終了を確認済み。
画像入力とoutputSchema、Chatの推論強度、モデル一覧のページ送りを追加した。
App Server異常終了時のProxy終了とsystemdによる再起動に対応した。完全なtool call／usage変換、長時間・高負荷検証などは未完了である。

制御API v1（要求IDの永続照会、Steer／中断、承認制御、snapshot SSE）と同一Provider内の会話モデル変更も実装し、実Codexで検証した。
現行の標準構成ではv2のSQLiteへ会話・実行・停止・成果物・リース・監査を保存し、旧v1台帳も移行する。JSONLのイベントジャーナルと移行元の退避は継続する。本文の旧Draftテーブル構想をそのまま実装したものではなく、現行構造は[v2実装設計](v2-system-design.ja.md)を参照する。
質問・MCP追加確認・権限専用承認は、対応を宣言したv2クライアント向けに[対話中継API](interaction-api.ja.md)を実装した。GatewayのUI実装・結合受入は未完了。未宣言・未対応の要求にはエラーを返し、対象Turnの停止を要求する。詳細と配備状況は[追加修正・受入記録](proxy-hardening-2026-09-13.ja.md)を参照する。
正確な契約・責務境界は[制御API v1](control-api.ja.md)と[v2契約](workspace-artifact-api-v2.ja.md)、適用済みの構成は[常駐設定](server-operations.md)を参照する。

---

## 2. 背景

Codex は Agent Loop、Shell 実行、ファイル編集、Tool Call、Sandbox、Approval、Thread 管理など、コーディングエージェントとして必要な機能を持つ。

一方、通常の Codex CLI や Codex アプリは、外部アプリケーションから常駐サービスとして利用するための OpenAI 互換 API を直接提供しない。

Codex App Server は、Codex の内部機能を JSON-RPC ベースで利用するための常駐インターフェースを提供する。しかし、そのプロトコルは OpenAI API 互換ではなく、OpenWebUI や OpenAI SDK から直接利用することはできない。

本プロジェクトでは、Codex App Server を常駐起動し、その前段に OpenAI 互換 API を提供する Proxy を配置する。

また、Codex の推論プロバイダとして以下を切り替えて利用できるようにする。

- ChatGPT
- Hoshikage
- Ollama

これにより、OpenWebUI や OpenAI SDK から、Codex の Agent Runtime を統一的に利用可能にする。

---

## 3. 目的

本システムの目的は、Codex App Server を常駐 Agent Runtime として利用し、外部へ OpenAI 互換 API を提供することである。

本システムは以下を実現する。

1. Codex App Server の常駐化
2. OpenAI Responses API の提供（最初の優先実装）
3. OpenAI Chat Completions API の提供
4. OpenWebUI からの利用
5. ChatGPT、Hoshikage、Ollama の切り替え
6. リクエスト単位のモデル切り替え
7. デフォルトモデルの設定
8. Codex Thread の継続利用
9. SSE ストリーミング
10. OpenWebUI 上での対話承認
11. Codex 固有イベントの保持
12. Provider 単位の並行実行制御
13. ChatGPT ProviderにおけるCodex準拠の推論レベル指定

本システムは、処理手順の集積ではなく、状態・観測・ルール・状態遷移の構造として拡張可能でなければならない。

---

## 4. 非目的

MVP では以下を対象外とする。

1. 旧 OpenAI Completions API `/v1/completions`
2. Chat Completions における Codex Thread の永続的継続
3. 継続中の Responses Thread におけるProviderを跨ぐモデル変更
4. Codex App Server 実行中 Turn の自動再送
5. 標準 OpenAI API ストリームへの Codex 独自イベント混入
6. 複数 Codex App Server のクラスタリング
9. 分散 Queue
10. マルチユーザー認証・権限管理

---

## 5. 用語

| 用語 | 定義 |
|---|---|
| Proxy | 本プロジェクトが実装する OpenAI 互換 API サーバー |
| Codex App Server | Codex Agent Runtime を JSON-RPC で提供する常駐プロセス |
| Provider | Codex が推論に利用するバックエンド |
| Public Provider ID | 外部 API で使用する Provider 名 |
| Codex Provider ID | Codex 内部設定で使用する Provider 名 |
| Public Model ID | 外部 API へ公開する `provider/model` 形式のモデル識別子 |
| Upstream Model ID | 実際に Provider へ渡すモデル名 |
| Model Selection | ProviderとModelを一体として解決した選択結果 |
| Reasoning Effort | ChatGPT Providerで指定するCodex準拠の推論レベル |
| Approval Capability | クライアントがApproval Requestを受け取り、決定を返せる能力 |
| Thread | Codex が保持する会話・作業セッション |
| Turn | Thread 内の一回のユーザー要求と Agent 実行 |
| Approval | Shell 実行やファイル変更などに対する承認 |
| SSE | Server-Sent Events。HTTP ストリーミング形式 |
| OpenWebUI Function | OpenWebUI に追加する連携用 Pipe Function |

---

## 6. システム境界

```text
┌──────────────────────────────────┐
│ OpenWebUI / OpenAI SDK           │
│                                  │
│ OpenWebUI Approval Pipe Function │
└────────────────┬─────────────────┘
                 │ OpenAI-compatible HTTP API
                 ▼
┌──────────────────────────────────┐
│ Codex Hoshikage Proxy            │
│                                  │
│ - API変換                        │
│ - Model解決                      │
│ - Thread対応管理                 │
│ - Approval管理                   │
│ - Provider並行制御               │
│ - Codexプロセス監視              │
└────────────────┬─────────────────┘
                 │ JSON-RPC / JSONL
                 ▼
┌──────────────────────────────────┐
│ Codex App Server                 │
│                                  │
│ - Agent Loop                     │
│ - Shell                          │
│ - File Edit                      │
│ - Tool Call                      │
│ - Sandbox                        │
│ - Approval                       │
│ - Thread                         │
└─────────┬──────────┬─────────────┘
          │          │
     ┌────▼────┐ ┌───▼──────┐ ┌──────────┐
     │ChatGPT  │ │Hoshikage │ │ Ollama   │
     └─────────┘ └──────────┘ └──────────┘
```

---

## 7. 責務分担

### 7.1 Proxy の責務

Proxy は以下を担当する。

- HTTP サーバー
- OpenAI API 互換 Wire の提供
- OpenAI API と Codex JSON-RPC の変換
- Provider とモデルの解決
- Model Selection と Reasoning Effort の解決
- デフォルトモデルの解決
- Codex App Server の起動と監視
- JSON-RPC Request/Response の相関管理
- Codex Event の振り分け
- SSE 生成
- 非 Streaming 応答の集約
- `response_id` と `thread_id` の対応管理
- Approval 状態管理
- OpenWebUI Function 向け Approval API
- Provider 単位の Semaphore
- エラー変換
- 設定ファイルから Codex 設定を生成
- 専用 `CODEX_HOME` の管理

### 7.2 Codex App Server の責務

Codex App Server は以下を担当する。

- Agent Loop
- Thread 管理
- Turn 管理
- Shell 実行
- ファイル編集
- Tool Call
- Sandbox
- Approval Request
- Codex Event の生成
- Provider との Responses API 通信

### 7.3 Provider の責務

Provider は以下を担当する。

- LLM 推論
- Responses API の提供
- ストリーミング出力
- Tool Call 生成
- モデル固有の推論処理

### 7.4 OpenWebUI Function の責務

OpenWebUI Function は以下を担当する。

- OpenWebUI から Proxy への要求送信
- Proxy の Streaming 応答表示
- Codex 実行状況の表示
- Approval Request の検出
- OpenWebUI 上での承認ダイアログ表示
- ユーザーの承認・拒否結果の Proxy への返却

---

## 8. 提供 API

### 8.1 API 一覧

| Method | Path | 用途 |
|---|---|---|
| GET | `/healthz` | Proxy の稼働状態確認 |
| GET | `/readyz` | Codex App Server を含む受付可能状態確認 |
| GET | `/v1/models` | 公開モデル一覧 |
| POST | `/v1/chat/completions` | Chat Completions API |
| POST | `/v1/responses` | Responses API |
| GET | `/v1/codex/approvals/{approval_id}` | Approval 状態取得 |
| POST | `/v1/codex/approvals/{approval_id}` | Approval 決定 |
| GET | `/v1/codex/turns/{turn_id}/events` | Codex 固有イベント取得 |
| GET | `/v1/codex/turns/{turn_id}/events/stream` | Codex 固有イベントの SSE 配信 |

### 8.2 対象外 API

以下は MVP 対象外とする。

```text
POST /v1/completions
```

---

## 9. Chat Completions API 要件

### 9.1 エンドポイント

```http
POST /v1/chat/completions
```

### 9.2 入力例

```json
{
  "model": "hoshikage/qwen3.5-9b-vision",
  "messages": [
    {
      "role": "user",
      "content": "このプロジェクトのテストを実行してください"
    }
  ],
  "stream": true,
  "metadata": {
    "codex.cwd": "/home/user/projects/example"
  }
}
```

### 9.3 Thread 方針

MVP では、Chat Completions の各リクエストごとに新規 Codex Thread を作成する。

受信した `messages` 全体を Codex へ入力文脈として渡す。

Chat Completions における会話履歴は、OpenWebUI またはクライアントが所有する。

### 9.4 Streaming

`stream=true` の場合、Chat Completions 互換 SSE を返す。

完了時は以下を返す。

```text
data: [DONE]
```

### 9.5 非 Streaming

`stream=false` または未指定の場合、Codex Turn 完了まで出力を集約し、単一 JSON として返す。

---

## 10. Responses API 要件

### 10.1 エンドポイント

```http
POST /v1/responses
```

### 10.2 入力例

```json
{
  "model": "hoshikage/qwen3.5-9b-vision",
  "input": "このプロジェクトのテストを実行してください",
  "stream": true,
  "metadata": {
    "codex.cwd": "/home/user/projects/example"
  }
}
```

### 10.3 新規 Thread

`previous_response_id` が指定されていない場合、新規 Codex Thread を作成する。

### 10.4 継続 Thread

`previous_response_id` が指定されている場合、SQLite から対応する Codex Thread を解決し、既存 Thread を継続する。

### 10.5 同じ会話のモデル変更

2026-09-11改定: 同一Provider内では次のTurnの`model`指定で変更可能とする。履歴を破棄せず同じThreadを継続する。モデル省略時は最後に開始確認できた選択を引き継ぐ。旧Responseのモデル記録は書き換えない。

Providerを跨ぐ切替は`409 cross_provider_model_change_unsupported`。実行中は`409 thread_busy`。拒否・結果不明・再起動時の扱いは[制御API v1](control-api.ja.md)を正とする。

### 10.6 Streaming

`stream=true` の場合、Responses API 互換 SSE を返す。

### 10.7 非 Streaming

`stream=false` または未指定の場合、Codex Turn 完了まで出力を集約し、単一 JSON として返す。

### 10.8 Cancellation

クライアント切断または明示的なキャンセルを検出した場合、MVPでは同じCodex Turnへcancelを発行する。Turn状態を `Cancelled` とし、Provider Permitを解放する。

---

## 11. Provider 要件

### 11.1 Public Provider ID

外部へ公開する Provider ID は以下とする。

```text
chatgpt
hoshikage
ollama
```

### 11.2 Codex Provider ID との対応

| Public Provider ID | Codex Provider ID |
|---|---|
| `chatgpt` | `openai` |
| `hoshikage` | `hoshikage` |
| `ollama` | `ollama` |

外部 API では `openai` を Provider ID として使用しない。

理由は、OpenAI API 互換形式と ChatGPT Provider の混同を避けるためである。

### 11.3 推論レベルの適用範囲

推論レベルは `chatgpt` Provider のみに適用する。

`hoshikage` および `ollama` では、クライアントから `reasoning.effort` が明示された場合、`unsupported_parameter` として拒否する。ProxyがCodex側へ既定値を設定する必要がある場合のみ、`medium` を使用する。

推論レベルの値は、選択されたChatGPTモデルがCodex/OpenAI仕様上サポートする値だけを受け付ける。モデルごとの対応値は、Codex App Serverの動的モデルカタログを正とする。静的モデル定義が存在する場合は、明示的な上書き・固定値として扱う。

---

## 12. Model 要件

### 12.1 Public Model ID

公開モデル ID は以下の形式とする。

```text
<provider>/<model>
```

例:

```text
chatgpt/gpt-example
hoshikage/qwen-example
ollama/coder-example
```

### 12.2 Public Model ID と Upstream Model ID

外部へ公開するモデル名と、Provider へ渡すモデル名は分離する。

```toml
[models."hoshikage/qwen-example"]
provider = "hoshikage"
upstream_model = "actual-upstream-model-name"
display_name = "Example Model"
```

通常のモデルは、モデル一覧要求またはモデルを使用する要求の直前にProviderのカタログから発見するため、静的なモデル定義は必須ではない。必要な場合だけ、公開名の別名や表示名、能力の固定値を設定ファイルで上書きする。

### 12.2.1 Model Selection と Reasoning Effort

ProviderとModelは `provider/model` のPublic Model IDとして一体に解決する。一方、Reasoning EffortはModel Selectionとは独立したリクエスト設定として扱う。

Responses APIでは、ChatGPT Providerに限り以下を受け付ける。

```json
{
  "model": "chatgpt/gpt-5.6-sol",
  "reasoning": {
    "effort": "high"
  }
}
```

許可される値はモデル定義に従う。GPT-5.6系では `none`、`low`、`medium`、`high`、`xhigh`、`max` を想定するが、すべてのモデルが全値をサポートするとは限らない。

推論レベル未指定時は、ChatGPTモデルの既定値を使用する。非ChatGPT Providerでは外部指定を受け付けず、Codex内部で既定値が必要な場合のみ `medium` を使用する。

### 12.3 デフォルトモデル

設定ファイルでデフォルトモデルを指定可能とする。

```toml
[defaults]
model = "chatgpt/gpt-5.6-luna"
```

ChatGPTの既定モデルは、Codexが対応値を広告している場合、推論レベル `low` を既定値として使用する。

既定モデルがカタログに現れなくても起動は継続する。要求時の再取得後もそのモデルを利用できない場合、Provider停止中なら `provider_unavailable`、Providerが応答しているがモデルを返さない場合は `model_not_found` を返す。

### 12.4 モデル省略

以下の場合、デフォルトモデルを使用する。

- `model` が未指定
- `model` が空文字列
- `model` が `default`

### 12.5 起動時上書き

デフォルトモデルは以下で上書き可能とする。

1. CLI 引数
2. 環境変数
3. 設定ファイル

優先順位は上記の順とする。

### 12.6 モデル一覧とカタログ統合

`GET /v1/models` は、Proxyが利用可能なモデルをOpenAI互換形式で返す。

MVPでは、モデルカタログを以下の順に収集・統合する。

1. Hoshikage `/v1/models` と詳細能力カタログ `/v1/hoshikage/models`
2. Ollama `/api/tags`
3. ChatGPT/Codex App Server `model/list`（Codex全体カタログから、他Providerで既知のモデルを除外）
4. Proxy設定ファイルの静的モデル定義（任意の上書き・別名）

静的定義と動的取得結果が同じPublic Model IDになる場合は、静的定義を優先する。Codex App Serverの`model/list`はProvider横断カタログとして扱い、Hoshikage/Ollamaのカタログで既知のUpstream Model IDはChatGPTへ重複登録しない。動的に発見したモデルもModel Resolverへ登録し、`model` へ指定して利用できるようにする。

Hoshikageの動的モデルは詳細能力カタログの`tools`を確認する。Codex Agent RuntimeはTool Callingを必須とするため、`tools=false`または詳細能力を取得できないHoshikageモデルはProxyの公開モデル一覧へ登録しない。モデル名から能力を推測してはならない。

カタログ取得は `GET /v1/models` およびモデルを使用する要求の直前に行う。HoshikageとOllamaのHTTPカタログ要求には5秒のタイムアウトを設ける。Providerのカタログ取得に失敗しても、取得できた他Providerのモデルを返し、一覧API全体を失敗させない。失敗したProviderの動的モデルを推測して登録することはしない。失敗したProviderの動的モデルは最新Registryから除外し、要求時には `provider_unavailable` を返す。失敗はEvent Journalまたは運用ログへ記録する。

外部へ公開するIDは必ず以下の形式とする。

```text
<public_provider_id>/<upstream_model_id>
```

動的カタログは要求ごとに取得する最新スナップショットとして扱う。MVPでは専用カタログDB・バックグラウンド更新・Provider間の高度な重複解決は行わない。

---

## 13. Model Resolver

モデル解決は以下の状態遷移として扱う。

```text
Requested Model
      │
      ├─ 未指定
      ├─ 空文字列
      └─ "default"
             │
             ▼
      Default Model
             │
             ▼
      Static Model Registry
             │
             ▼
      Resolved Model
```

解決結果は以下を含む。

```rust
pub struct ResolvedModel {
    pub public_model_id: PublicModelId,
    pub public_provider_id: PublicProviderId,
    pub codex_provider_id: CodexProviderId,
    pub upstream_model_id: UpstreamModelId,
}
```

---

## 14. Codex App Server 要件

### 14.1 起動方式

Codex App Server は Proxy 起動時に一度だけ起動し、常駐させる。

MVP では `stdio` transport を使用する。

```text
codex app-server --listen stdio://
```

対応対象のCodex CLIは `0.147.0` 以降とする。互換性検証では、対象バージョンを固定したFixtureと実機テストを用いる。

### 14.2 初期化

Codex App Server 起動後、Proxy は以下を一度だけ実行する。

1. `initialize`
2. `initialized`

### 14.3 JSON-RPC

Proxy は以下を管理する。

- Request ID
- Response 相関
- Notification
- Thread ID
- Turn ID
- Item ID
- Approval ID

### 14.4 Supervisor

Codex App Server は Supervisor により監視する。

Codex App Server が停止した場合:

1. `/healthz` を unhealthy とする
2. `/readyz` を not ready とする
3. 実行中 Turn を失敗終了とする
4. Codex App Server を再起動する
5. `initialize` を再実行する
6. 復旧後に ready とする

### 14.5 自動再送禁止

実行中 Turn は自動再送しない。

理由は、Shell 実行やファイル変更などの副作用が二重実行されることを防ぐためである。

---

## 15. Thread 管理要件

### 15.1 会話履歴の所有者

Codex Thread の会話履歴は Codex が所有する。

Proxy は会話内容を重複保存しない。

### 15.2 対応情報

Proxy は以下の対応情報を SQLite に保存する。

```text
response_id → thread_id
```

### 15.3 保存期間

時間経過による自動削除は MVP では行わない。

Proxy再起動後は、SQLiteに保存されたThread IDで `thread/resume` を試みる。Codex App Server側でThreadが利用不能な場合は `thread_not_found` を返し、Proxyが会話内容からThreadを擬似復元することは行わない。

Codex Thread が削除または参照不能になった場合、対応レコードを孤立レコードとして削除可能とする。

### 15.4 SQLite スキーマ

最低限、以下を保持する。

```sql
CREATE TABLE response_threads (
    response_id      TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    public_model_id  TEXT NOT NULL,
    created_at       TEXT NOT NULL
);
```

---

## 16. 並行実行要件

### 16.1 Codex Thread

Codex App Server 側では複数 Thread の並行実行を許可する。

### 16.2 Provider Semaphore

並行実行数は Provider 単位の Semaphore で制御する。

```toml
[providers.chatgpt]
max_concurrent_turns = 4

[providers.hoshikage]
max_concurrent_turns = 1

[providers.ollama]
max_concurrent_turns = 1
```

### 16.3 既定値

| Provider | 既定値 |
|---|---:|
| ChatGPT | 設定可能 |
| Hoshikage | 1 |
| Ollama | 1 |

### 16.4 Permit 保持期間

Provider Permit は Turn 開始直前に取得し、Turn 完了、失敗、キャンセルのいずれかまで保持する。

このPermitはProviderが実際に推論中かどうかではなく、Codex Turn全体のProvider枠を占有するTurn-lifetime permitである。Approval待ち中も保持する。Approval待ち中のPermit解放と再取得は、待機時間が長いProviderの詰まりを改善するPhase 2候補とする。

---

## 17. Approval 要件

### 17.1 承認ポリシー

Approval Policy は設定可能とする。

```toml
[approval]
policy = "interactive"
auto_approve_workspace = true
timeout_seconds = 300
```

### 17.2 自動承認

Codexが構造化された `cwd`、`path`、`file_path`、`filePath`、`target_path`、`targetPath` のいずれかで、リクエストされたcwd内の操作として報告したものは、自動承認可能とする。これには、cwd配下に置いた `.codex/skills` や `.agents/skills` の操作も含む。コマンド文字列にcwdの文字列が含まれているだけでは自動承認しない。許可ルート外の操作は自動承認しない。

### 17.3 対話承認

自動承認対象外の操作は、OpenWebUI 上で対話承認する。

### 17.4 Approval Flow

```text
Codex Approval Request
        │
        ▼
Proxy Approval State = Pending
        │
        ▼
OpenWebUI Function
        │
        ▼
Confirmation Dialog
        │
        ├─ Approve
        └─ Deny
        │
        ▼
Proxy Approval API
        │
        ▼
Codex App Server
```

### 17.5 Approval API

承認状態取得:

```http
GET /v1/codex/approvals/{approval_id}
```

承認・拒否:

```http
POST /v1/codex/approvals/{approval_id}
```

Request:

```json
{
  "decision": "accept"
}
```

または:

```json
{
  "decision": "accept_for_session"
}
```

拒否またはキャンセルの場合:

```json
{
  "decision": "decline"
}
```

```json
{
  "decision": "cancel"
}
```

Approval APIのWire Decisionは、Codexの `availableDecisions` と同じ4値を使用する。Proxyは `accept`、`accept_for_session`、`decline`、`cancel` をDomainの `Accept`、`AcceptForSession`、`Decline`、`Cancel` へ変換し、Codexへ返す。OpenWebUI v0.11.0の標準Confirmation Dialogは二択のため、PipeはUI上の承認を `accept`、キャンセルを `decline`（提示されない場合は `cancel`）へAdapter変換する。

### 17.6 Approval State

```rust
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Cancelled,
}
```

### 17.7 Timeout

設定時間内に承認されなかった場合、Approval は `Expired` とする。

Codex へ拒否を返し、Turn を失敗終了とする。

OpenWebUI v0.11.0の標準Pipe経路では、Proxyがタイムアウト処理を完了しても、
OpenWebUI標準のConfirmation DialogをPipe／Proxyから閉じるUIイベントが存在しない。
そのため、ブラウザ上の確認ダイアログが残ることがある。これはOpenWebUI本体を改修しない
前提での既知の運用制約であり、ダイアログが残っていてもApprovalは既に`Expired`、Codexへは
拒否応答済み、Turnは終端状態である。残ったダイアログから後追いで送信されたDecisionは
受け付けない。

### 17.8 Approval capabilityを持たないクライアント

OpenWebUI PipeなどApproval capabilityを持たないクライアントでは、Approval Request発生時にHTTP応答だけを先に返してはならない。以下のCleanup Flowを完了させた後、`approval_required` を返す。

Approval capabilityはリクエストコンテキストで明示する。OpenWebUI Pipeは `metadata["codex.approval_capability"] = "interactive"` を付与し、それ以外のクライアントは capabilityなしとして扱う。

```text
Approval Request
    ↓
client capability = none
    ↓
Codexへ Cancel を返す
    ↓
Approval = Cancelled
    ↓
Turn = Cancelled
    ↓
Provider Permitを解放
    ↓
HTTP approval_required
```

自動承認は行わない。`approval_required` は、ユーザーが拒否したことではなく、クライアントが対話承認を提供できないことを表す。

非StreamingではCleanup完了後にHTTP `409`とOpenAI互換エラーを返す。Streamingでは、SSEヘッダー送信前なら同じHTTP `409`を返し、送信後なら標準エラーイベントとして `approval_required` を配信してストリームを終了する。

---

## 18. OpenWebUI Function 要件

### 18.1 配布物

MVP には OpenWebUI 用 Pipe Function を同梱する。

```text
openwebui/
└── codex_hoshikage_proxy_pipe.py
```

### 18.2 機能

OpenWebUI Function は以下を実装する。

- Proxy への Chat Completions 要求
- SSE ストリーミング
- Codex 状態表示
- Approval Request 検出
- Confirmation Dialog 表示（二択UIを4値Wire DecisionへAdapter変換）
- Approval API 呼び出し
- 拒否・タイムアウト表示

OpenWebUI v0.11.0の標準Pipeでは、Approval timeout後にConfirmation Dialogが自動的に
閉じない場合がある。Pipeの対応範囲はProxyへのCleanupと状態通知までとし、ダイアログの
強制終了は行わない。

### 18.3 通常クライアント

OpenWebUI Function を使用しない通常の OpenAI 互換クライアントでも、標準 API は利用可能とする。

ただし対話承認 UI は提供されない。

---

## 19. cwd 要件

### 19.1 指定方法

作業ディレクトリは OpenAI API の `metadata` で指定する。

```json
{
  "metadata": {
    "codex.cwd": "/home/user/projects/example"
  }
}
```

### 19.2 優先順位

```text
request.metadata["codex.cwd"]
    ↓
model.default_cwd
    ↓
server.default_cwd
```

### 19.3 検証

`cwd` は以下を満たす必要がある。

- 絶対パス
- 正規化可能
- 設定された許可ルート内
- 必要に応じて存在確認可能

許可ルートは設定されたAllowlistで管理する。MVPの既定例は以下とする。

```toml
[security]
allowed_cwds = [
    "${HOME}/work",
    "${HOME}/projects",
]
api_key = "change-me"
```

指定された `cwd` は絶対パスで、正規化後にAllowlist内であり、実在する場合だけ受け付ける。Proxyは存在しないディレクトリを作成しない。

不正な場合は `invalid_cwd` を返す。

### 19.4 公開リポジトリ

GitHub 掲載物には以下を含めない。

- 個人名
- 個人ユーザー名
- 固有ホームディレクトリ
- 個人環境の固定パス
- 個人用 API キー
- 個人用 Token

例示パスには以下を使用する。

```text
/home/user/projects/example
${HOME}/projects/example
```

---

## 20. Sandbox 要件

### 20.1 許可値

```text
read-only
workspace-write
```

Proxyは安全境界を維持するため `read-only` と `workspace-write` のみを設定可能とする。`danger-full-access` はProxy設定から指定できない。

### 20.2 既定値

```text
workspace-write
```

### 20.3 型検証

Sandbox 値は列挙型として検証する。

未定義文字列を Codex App Server へ渡してはならない。

---

## 21. Codex 固有イベント要件

### 21.1 Codex 固有イベント

Codex は以下のイベントを生成し得る。

- Shell command started
- Shell command completed
- Shell output
- File change
- Tool call
- Approval request
- Subagent started
- Subagent completed
- Reasoning
- Agent message
- Turn completed
- Turn failed

### 21.2 標準 API 投影

`/v1/chat/completions` および `/v1/responses` では、各 API の標準スキーマへ変換可能な情報のみ返す。

### 21.3 独自イベント混入禁止

標準 OpenAI API の SSE へ、未知の Codex 独自イベントを混入しない。

### 21.4 内部保持

Codex 固有イベントは内部イベントとして保持する。

Event Journalは日次またはサイズ上限でローテーションし、保持期間を設定可能とする。Shell出力やファイル内容は無制限に保存せず、既定ではmetadata中心とし、保存する場合もサイズ制限とredactionを適用する。MVPでは専用Indexを設けず、JSONLと標準CLIツールで検索する。

### 21.5 拡張 API

Codex 固有イベントは以下から参照可能とする。

```http
GET /v1/codex/turns/{turn_id}/events
GET /v1/codex/turns/{turn_id}/events/stream
```

### 21.6 OpenWebUI 表示

OpenWebUI Function は拡張イベント API を利用し、必要に応じて以下を表示可能とする。

- 実行中コマンド
- ファイル変更
- テスト進行
- Approval Request
- Turn 状態

公式Pipe実装はOpenWebUI v0.11.0を対象とし、`openwebui/codex_hoshikage_pipe.py`として同梱する。Pipeは`/v1/models`からモデル一覧を取得し、Chat CompletionsのTurn IDを拡張イベントSSEへ接続してApprovalを`__event_call__`へ投影する。

---

## 22. 設定要件

### 22.1 設定ルート

```text
~/.config/codex-hoshikage-proxy
```

### 22.2 ディレクトリ構成

```text
~/.config/codex-hoshikage-proxy/
├── config.toml
├── codex-home/
│   ├── config.toml
│   ├── auth.json
│   └── sessions/
└── state/
    └── proxy.sqlite3
```

### 22.3 環境変数

設定ルートは以下で変更可能とする。

```text
CODEX_HOSHIKAGE_PROXY_HOME
```

### 22.4 専用 CODEX_HOME

Proxy は専用 `CODEX_HOME` を使用する。

```text
~/.config/codex-hoshikage-proxy/codex-home
```

既存の個人用 Codex 環境を読み書きしない。

### 22.5 Codex 設定生成

Proxy は `config.toml` の Provider 定義から、専用 `CODEX_HOME/config.toml` の `model_providers` 定義を生成する。

専用 `CODEX_HOME` はProxy管理領域とする。`config.toml`は宣言的設定から毎回再生成し、手編集はサポートしない。一方、`auth.json`はProxyが生成せず、ユーザーがCodex標準の認証手順で用意する。

---

## 23. 設定ファイル例

```toml
[server]
host = "127.0.0.1"
port = 4040
default_cwd = "${HOME}/projects"

[security]
allowed_cwds = [
    "${HOME}/work",
    "${HOME}/projects",
]

[codex]
command = "codex"
args = ["app-server", "--listen", "stdio://"]

[codex.sandbox]
mode = "workspace-write"
writable_roots = []
network_access = false

[defaults]
model = "hoshikage/unsloth-gemma4-12b-qat-thinking-off"

[approval]
policy = "interactive"
auto_approve_workspace = true
timeout_seconds = 300

[providers.chatgpt]
codex_provider = "openai"
enabled = true
max_concurrent_turns = 4

[providers.hoshikage]
codex_provider = "hoshikage"
enabled = true
base_url = "http://127.0.0.1:3030/v1"
max_concurrent_turns = 1

[providers.ollama]
codex_provider = "ollama"
enabled = true
base_url = "http://127.0.0.1:11434/v1"
max_concurrent_turns = 1

[models."chatgpt/gpt-example"]
provider = "chatgpt"
upstream_model = "gpt-example"
display_name = "ChatGPT Example"
reasoning_efforts = ["none", "low", "medium", "high", "xhigh", "max"]
default_reasoning_effort = "medium"

[models."hoshikage/unsloth-gemma4-12b-qat-thinking-off"]
provider = "hoshikage"
upstream_model = "unsloth-gemma4-12b-qat-thinking-off"
display_name = "Hoshikage Gemma 4 12B Thinking Off"

[models."ollama/coder-example"]
provider = "ollama"
upstream_model = "coder-example:latest"
display_name = "Ollama Example"
```

---

## 24. 認証要件

### 24.1 Proxy API

Loopback bindではAPI Key認証を任意とする。非loopbackへListenする場合はAPI Key認証を必須とする。

MVPは単一API Keyに対応する。`security.api_key`へ直接記載でき、`security.api_key_env`を指定した場合は環境変数からの取得も可能とする。両方が指定された場合は`api_key`を優先する。複数Keyと権限管理はPhase 2とする。TLSはリバースプロキシへ委譲し、CORSはデフォルト無効とする。OpenWebUI Pipeには同じAPI KeyをSecretまたは環境設定から渡す。

### 24.2 ChatGPT 認証

ChatGPT Provider の認証情報は専用 `CODEX_HOME` 配下で管理する。

既存の個人用 Codex 認証情報を暗黙に流用しない。

ChatGPT Providerを利用する場合、ユーザーはProxy専用`CODEX_HOME`でCodex標準ログインを実行する。

```bash
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login --device-auth
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login status
```

`--with-api-key`はChatGPT Plus認証ではなくAPI Key認証なので、Plusプラン利用のProxy設定では使用しない。

### 24.3 Secret

以下をログへ出力してはならない。

- API Key
- Bearer Token
- ChatGPT 認証情報
- Provider Token
- Cookie
- Authorization Header

---

## 25. エラー要件

### 25.1 エラー形式

OpenAI 互換エラー形式を返す。

```json
{
  "error": {
    "message": "Requested model was not found.",
    "type": "invalid_request_error",
    "param": "model",
    "code": "model_not_found"
  }
}
```

### 25.2 エラー一覧

| Code | HTTP | 内容 |
|---|---:|---|
| `invalid_request_error` | 400 | 不正な入力 |
| `invalid_cwd` | 400 | 不正な作業ディレクトリ |
| `model_not_found` | 404 | モデル未登録 |
| `response_not_found` | 404 | Response 対応情報なし |
| `thread_not_found` | 404 | Codex Thread なし |
| `cross_provider_model_change_unsupported` | 409 | Providerを跨ぐ継続中モデル変更 |
| `approval_denied` | 403 | 承認拒否 |
| `approval_required` | 409 | クライアントにApproval capabilityがなく、対話承認が必要 |
| `approval_timeout` | 408 | 承認タイムアウト |
| `provider_unavailable` | 503 | Provider 利用不可 |
| `codex_not_ready` | 503 | Codex 初期化未完了 |
| `server_overloaded` | 503 | Queue または Codex 過負荷 |
| `upstream_timeout` | 504 | Provider 応答タイムアウト |
| `turn_failed` | 500 | Codex Turn 失敗 |
| `codex_process_terminated` | 503 | Codex App Server 停止 |
| `unsupported_parameter` | 400 | Providerまたはモデルが対応しないパラメータ |

### 25.3 Retry

副作用を伴う Agent Turn は自動再試行しない。

HTTP クライアントへ retryable 情報を返す場合でも、自動再送はクライアント判断とする。

---

## 26. 状態遷移要件

### 26.1 Proxy Runtime

```text
Stopped
  ↓ start
Starting
  ↓ process spawned
Initializing
  ↓ initialize completed
Ready
  ↓ process exited
Recovering
  ↓ restart completed
Initializing
```

### 26.2 Turn

```text
Created
  ↓ provider permit acquired
Starting
  ↓ Codex turn started
Running
  ├─ approval requested → AwaitingApproval
  ├─ completed          → Completed
  ├─ failed             → Failed
  ├─ interrupted        → Interrupted
  └─ cancelled          → Cancelled

AwaitingApproval
  ├─ approved → Running
  ├─ denied   → Failed
  ├─ timeout  → Failed
  └─ cancelled → Cancelled
```

### 26.3 Response

```text
Created
  ↓
InProgress
  ├─ Completed
  ├─ Failed
  └─ Cancelled
```

---

## 27. 非機能要件

### 27.1 実装言語

Proxy 本体は Rust を推奨する。

OpenWebUI Function は Python で実装する。

### 27.2 Web Framework

Rust 側 HTTP サーバーは Axum を推奨する。

### 27.3 非同期 Runtime

Tokio を使用する。

### 27.4 永続化

SQLite を使用する。

### 27.5 ログ

構造化ログを採用する。

ログには最低限以下を含める。

- request_id
- response_id
- thread_id
- turn_id
- provider_id
- public_model_id
- approval_id
- error_code

### 27.6 Redaction

Secret、入力本文、Shell 出力、ファイル内容は設定なしに全文ログへ出してはならない。

### 27.7 可用性

Codex App Server 停止時は Supervisor により自動復旧する。

### 27.8 安全性

既定 Sandbox は `workspace-write` とする。

### 27.9 互換性

OpenAI-compatible subsetとして提供する。MVPの主要対応範囲は以下とする。

| API | 対応フィールド |
|---|---|
| Chat Completions | テキスト／画像`messages`, `model`, `stream`, `metadata`, `response_format`, `reasoning_effort`（ChatGPTのみ）、テキスト差分・完了／失敗、切断時の中断 |
| Responses | テキスト／画像／メッセージ形式`input`, `previous_response_id`, `model`, `stream`, `metadata`, `text.format`, `reasoning`（ChatGPTのみ）、テキスト差分・完了／失敗、切断時の中断 |

`tools`、`tool_choice`、Function Call、multimodal contentなどの詳細対応は、各Wire Adapterの対応表で明示する。未対応または意味を安全に無視できないフィールドは、黙って無視せず `unsupported_parameter` とする。

### 27.10 Portable Configuration

GitHub 公開物は個人環境に依存しないこと。

### 27.11 構造と状態遷移

以下を設計上の非機能要件とする。

1. Runtime、Thread、Turn、Approval、ResponseのDomain Stateは不変スナップショットとして扱う。
2. Domain StateはProcess、Channel、Semaphore、SQLite接続、HTTP Bodyなどの副作用資源を所有しない。
3. 状態変更は `State + Event → Next State + Effects` として表現する。
4. Ruleは状態を直接変更せず、合法なTransitionを生成する。
5. 複数の合法な遷移がある場合は、遷移候補を遅延生成できる構造とする。
6. Queryは状態の観測だけを担当し、状態変更を行わない。
7. Application ServiceはTransitionを適用し、Effectsを実行するオーケストレーションに限定する。
8. 新しいProvider、Approval方式、Codex Event、状態変化は、既存Handlerへの条件分岐追加ではなく、対応する型・Rule・Adapterとして追加する。

---

## 28. セキュリティ要件

1. API Listen Address の既定値は `127.0.0.1`
2. 外部公開は明示設定時のみ
3. `danger-full-access` は明示設定時のみ
4. `cwd` は許可ルート検証を行う
5. Path Traversal を防止する
6. Approval API は推測困難な ID を使用する
7. Approval の二重回答を禁止する
8. Secret をログへ出さない
9. Codex App Server の stdin/stdout を外部へ直接公開しない
10. 専用 `CODEX_HOME` を利用する

---

## 29. テスト要件

### 29.1 Unit Test

以下を単体テストする。

- Model Resolver
- ChatGPT ModelごとのReasoning Effort検証
- 非ChatGPT ProviderへのReasoning Effort拒否
- Provider Mapping
- Default Model Resolution
- Sandbox Validation
- cwd Validation
- Error Mapping
- Approval State Transition
- Turn State Transition
- SSE Serialization
- SQLite Repository
- Domain Stateの不変性
- Queryの副作用なし
- RuleからのTransition生成
- Effect実行結果からのDomain Event復帰
- ApprovalAccepted / ApprovalDeclined / ApprovalExpired / ApprovalCancelledのTurn遷移

### 29.2 Fake Codex App Server

テスト用 Fake App Server を用意する。

以下を再現可能とする。

- initialize
- thread/start
- thread/resume
- turn/start
- agent message delta
- command event
- approval request
- turn completed
- turn failed
- process termination
- overloaded error

### 29.3 Integration Test

以下を統合テストする。

1. Chat Completions 非 Streaming
2. Chat Completions Streaming
3. Responses 非 Streaming
4. Responses Streaming
5. previous_response_id 継続
6. 同一Provider内モデル変更・Provider間変更拒否
7. Provider 切り替え
8. Provider Semaphore
9. Approval Approve
10. Approval Deny
11. Approval Timeout
12. Approval capabilityなしクライアントのCleanup Flow
13. Codex 再起動
14. `/v1/models`
15. OpenWebUI Function

---

## 30. 受入基準

MVP は以下をすべて満たした場合に受入可能とする。

### 30.1 起動

- Proxy 起動時に Codex App Server が常駐起動する
- initialize が一度だけ正常完了する
- `/healthz` が成功する
- `/readyz` が成功する

### 30.2 OpenWebUI

- OpenWebUI からモデル一覧を取得できる
- OpenWebUI から Chat Completions を実行できる
- Streaming 表示できる
- Approval Dialog を表示できる
- 承認後に同じ Turn が継続する
- 拒否時に適切なエラー表示となる

### 30.3 Provider

- ChatGPT を選択できる
- Hoshikage を選択できる
- Ollama を選択できる
- リクエストごとに Provider とモデルを切り替えられる

### 30.4 Default Model

- `model` 省略時に既定モデルが使用される
- `model="default"` で既定モデルが使用される
- 未登録モデルは `model_not_found` となる

### 30.5 Responses

- 新規 Response が新規 Thread を作る
- `previous_response_id` で同一 Thread を継続できる
- 同一Provider内のモデル変更は文脈を維持し、Provider間変更は明示的に拒否される
- Proxy 再起動後も SQLite から Thread 対応を復元できる

### 30.6 並行制御

- Hoshikage の既定同時 Turn 数が 1
- Ollama の既定同時 Turn 数が 1
- ChatGPT の同時 Turn 数を設定できる

### 30.7 復旧

- Codex App Server 停止を検出できる
- 実行中 Turn を自動再送しない
- Codex App Server を再起動できる
- 再初期化後に新規要求を受け付けられる

### 30.8 公開品質

- 個人名を含まない
- 個人パスを含まない
- Secret を含まない
- サンプル設定だけで基本構成を理解できる
- GitHub 上で Markdown として正常表示できる

---

## 31. Phase 2 候補

1. カタログのバックグラウンド先読み（要求時更新を補完する任意機能）
2. 専用カタログDB
3. Provider間の高度な重複解決
4. Chat Completions と Codex Thread の継続連携
5. Thread Fork によるモデル切り替え
6. 複数 Codex App Server
7. Multi-user
8. 複数API Key・権限管理
9. Web 管理 UI
10. Metrics
11. OpenTelemetry
13. Approval 履歴 UI
14. Provider Failover
15. Model Routing

---

## 32. 現時点での決定事項

MVP の主要決定事項は以下とする。

1. Codex App Server を `stdio` で常駐起動する
2. OpenAI Chat Completions API と Responses API を提供する
3. OpenWebUI に対応する
4. OpenWebUI 上で対話承認を可能にする
5. Provider は `chatgpt`、`hoshikage`、`ollama`
6. Public Model ID は `provider/model`
7. モデル一覧は設定ファイルとProviderカタログを統合して管理する
8. Providerカタログ取得はモデル一覧要求またはモデル使用要求の直前に行い、静的モデル定義は任意の上書き・別名として適用する
9. `model` は省略可能
10. `default` は設定された既定モデルへ解決する
11. Responses の継続中モデル変更は同一Provider内で次のTurnから適用する
12. Chat Completions はリクエストごとに新規 Thread を作る
13. Response と Thread の対応を SQLite へ保存する
14. 対応情報は時間経過で自動削除しない
15. Provider 単位の Semaphore を持つ
16. Hoshikage と Ollama の既定同時実行数は 1
17. `workspace-write` を既定 Sandbox とする
18. 安全な workspace-write 操作は自動承認可能
19. それ以外は OpenWebUI で対話承認する
20. `cwd` は `metadata["codex.cwd"]` で指定する
21. Codex 固有イベントは標準 API へ混入させない
22. Codex 固有イベントは拡張 API で取得可能とする
23. Codex App Server 停止時は自動再起動する
24. 実行中 Turn は自動再送しない
25. 設定ルートは `~/.config/codex-hoshikage-proxy`
26. 専用 `CODEX_HOME` を管理する
27. GitHub 公開物に個人名、個人パス、Secret を含めない
28. 最終機能を網羅する前提で、Responses APIを最初の優先実装とする
29. OpenWebUI対応は後段で実装するが、最終要件として維持する
30. Codex CLI `0.147.0` 以降を対応対象とする
31. 推論レベルはChatGPT Providerのみに適用する
32. 推論レベルはモデルごとのCodex対応値を検証する
33. 非ChatGPT Providerで明示された推論レベルは `unsupported_parameter` とする
34. 非ChatGPT ProviderでCodex内部の既定値が必要な場合は `medium` とする
35. Domain State、Query、Rule、Transition、Effectを分離する
36. Domain Stateは不変スナップショットとして扱う
37. 機能追加は既存Handlerへの条件分岐追加ではなく、型・Rule・Adapterの追加で行う
38. Approvalの拒否・タイムアウト・キャンセルはTurnへ意味別Eventとして伝播する
39. Approval capabilityなしの場合はCleanup完了後に `approval_required` を返す
40. Provider PermitはTurn-lifetime permitとしてApproval待ち中も保持する
41. OpenWebUI Pipeは `metadata["codex.approval_capability"] = "interactive"` を付与する
42. Streaming開始後の `approval_required` はSSEエラーイベントとして返す
43. Approval APIは `accept`、`accept_for_session`、`decline`、`cancel` の4値をWire契約とする

---

以上を Codex Hoshikage Proxy の現時点の要件Draftとする。未実装の項目は段階的に実装・検証し、Verifiedへ更新する。

## MCPターン限定許可（2026-09-16 方針合意・具体契約提示）

単発Allowからの自動昇格を禁止する。Gatewayが認証する利用者・会話境界を汎用識別子として受け、run_id／Response／Turn／入力世代／MCPサーバー／ツール／設定世代に許可を拘束する。別Run・別利用者・別会話への持越しを禁止する。Steerを含む次の利用者発言、stop/cancel、Run終了、再起動、設定変更、期限切れで失効する。承認ボタンは追加発言ではない。

安定した呼出しIDと信頼できる発生元により、承認要求と実呼出しを確実に対応付ける。対応付けできるがターン許可対象外の場合は単発mcp_formを維持する。対応付けできない要求は元のuser_inputとして扱い、未宣言クライアントではunsupported_interactionで対象実行を停止する。承認の種類を推測して変換しない。`browser_run_code_unsafe` は常に個別確認。`browser_evaluate` は実証対象とし、実行コードと危険度を検証する。操作内容の説明は独立した必須受入条件とする。内部フォーム・認証・権限追加をターン許可で回答しない。

進行順は上流実証、具体API・状態遷移、並列／取消／世代変更を含む受入。常駐環境の自動許可有効化は今回の実装指示には含まない。


具体契約の正本は[MCP操作詳細・単一Run限定許可 API 0.3](mcp-turn-approval-api.ja.md)。Gatewayレビュー5項目への回答、型・サイズ・完全応答例、状態遷移、A01〜A11の受入条件を含む。生引数の保持は最長10分、許可も最大10分・Run内まで。監査メタデータと冪等操作記録は基本v2契約の明示状態廃止まで保持する。許可TTLと監査保持を混同しない。

ドキュメント完成前に着手した実装は未完了として保持する。実装再開条件は、要件・設計・具体契約・受入条件の整合確認、完成版提示、Gateway接続上の指摘解消。2026-09-16のGatewayによる0.3受入で、この開始条件は充足した。[開発工程](development-process.ja.md)を適用する。文書の完成は機能受入済みを意味しない。

## MCP公開カード承認（2026-09-16、0.4接続レビュー案）

[公開カード承認API 0.4案](mcp-inline-approval-api.ja.md)を追加要求の正本とする。0.3の常駐実装とは区別し、接続レビュー前に実装しない。

- 依頼元の会話で公開可能な操作・対象・制限と許可選択を同じカードにまとめる。本人限定詳細を開くことを通常フローの必須操作にしない。DMへ変更しない。
- 公開表示を許可することと、ツール実行の単発／ターン許可を分離する。元の会話へ表示する指定だけで任意の秘密引数の公開に同意したと解釈しない。
- Proxyは評価済みの公開表示生成規則を持ち、既存requester_onlyの引数を公開へ転用しない。公開可否・承認判断に十分か・ターン許可適格性を独立に判定する。
- 公開内容が不足、秘匿、長文、未知の形式の場合は許可を出さず、理由付き本人限定補足または取得不能へ移る。省略された説明で許可を誘導しない。
- 表示トークンをcall／scope／revision／投稿先／公開方針／表示内容に拘束する。Gatewayの本人認可・表示版保存と、Proxyの送信意思保存時の再検証を両方必須にする。
- browser_evaluateは個別確認を維持する。許可対象の追加や上流の常時許可設定変更は本改訂に含めない。許可一覧・取消はGatewayの /mcp に分離する。
- 利用者決定に基づき検索語・regex・アクセス先URLは元会話へ表示する。認証情報・秘密入力・任意コードは本人限定補足。browser_find／browser_navigateの通常操作を初回カードで承認できることを必須とし、全件補足へ戻す実装を完了としない。具体renderer・除外規則は0.4の第5節を正本にする。

受入は0.4のI01〜I11。0.3のA01〜A11と失効・停止・冪等性・互換要件を維持し、実Discordの利用者操作まで確認する。

## MCP承認の本格是正（2026-09-17）

0.4の配備は通常MCP操作全体の要求を満たしていなかった。既存3形式のみの復旧を先行リリースする案は撤回する。以下を単一の是正範囲とし、全体設計・接続レビュー後に実装する。

| ID | 要求 | 設計・受入 |
| --- | --- | --- |
| MFR-01 | 全MCP/Appsを有効にした構成で、カタログのサイズ・時間・ページングを有界に処理する。個別Threadの実定義へ対応付ける | カタログ設計／F02・F08 |
| MFR-02 | 342件全ての実定義・引数を台帳へ記録し、汎用表示・単発承認の完全性を検証する。意味評価済みの範囲は別に管理し、依頼中許可だけをその範囲へ限定する。未評価を安全性評価済みと表示しない | 全ツール台帳／F01・F03 |
| MFR-03 | 通常操作は初回カードで実操作・対象・指定条件の全体と適切な選択肢を提示する。秘密等の例外でも公開可能な固定概要を示す | API・投影設計／F01・F03・F07 |
| MFR-04 | timeout、RPC失敗、上限、定義不一致、未対応形式、秘密、表示超過を区別する | API reason／F02・F03 |
| MFR-05 | 外部送信・共有権限変更・削除は毎回確認する。他の評価済み通常操作は依頼中許可を提供する | 利用者決定2026-09-17／F04・F05 |
| MFR-06 | 任意コード・秘密入力・内部認証は従来どおり個別確認。単発Allowを包括許可へ昇格しない | F04・F05 |
| MFR-07 | ターン許可の適用時も実呼出しと全引数の作用を再検証する。複合ツールの例外操作が既存許可を通過しない | ポリシー設計／F05 |
| MFR-08 | 設定と推奨対象、UIの2択／3択、マニュアルを一致させる。browser_findだけの試験設定を製品既定と混同しない | 設定移行・運用設計／F04 |
| MFR-09 | 同一RunでもSteer前に失効。Stop・取消・終了・世代変更・復元で失効、別会話・次依頼に引き継がない | F06 |
| MFR-10 | クライアントの本人・会話・完全な表示・配信確定・版照合を必須とする。Discordでの具体実現はGatewayが担当する | Gateway責務／F07 |
| MFR-11 | 定義試験、合成試験、実カタログ、実ツール、実Discordを区別して全体の受入を記録する | 検証記録／F01〜F08 |

公開範囲は下記の利用者決定を適用する。通常の非機密な投稿文・編集本文・入力値は全文表示し、個人情報・連絡先・DM／メール・認証関連は、押した本人だけに見えるメッセージで確認する。

参照：[全体契約検討](mcp-approval-full-fix-api.ja.md)、[全ツール台帳](mcp-approval-tool-coverage.ja.md)、[検証記録](mcp-approval-full-fix-validation.ja.md)。本節の追記を実装・本番修正完了とは扱わない。

### 公開範囲・初回カードの追加決定（2026-09-17）

利用者は「通常の非機密入力は公開、個人情報・連絡先・DM/メール・認証関連は本人限定」と決定した。通常の投稿文・編集本文・フォーム入力値も、同条件で元会話へ全文表示する。上記の公開範囲に関する回答待ちは解消。未知の個人情報を完全識別できるとの保証を置かず、構造・用途・内容を検証できない入力は本人限定にする。

MFR-12：通常操作の初回カードは操作内容と実際の許可選択を同時に提供する。「本人限定／拒否」だけの入口を通常フローとして合格にしない。外部送信・共有変更・削除は「今回だけ許可／拒否」、その他の適格な通常操作は「今回だけ許可／この依頼中、このツールを許可／拒否」。本人限定画面は合意されたプライバシー・秘密等の例外に使用し、取得障害や未実装の代替として正常扱いしない。

元会話で公開不可な操作も、本人限定画面では内容の確認と実際の許可・拒否を同じ画面で行う。開く操作を承認と解釈しない。Gatewayが非公開内容を公開カードへコピーする対応では解決しない。

本人限定の確認ボタンは「自分だけに表示して確認」と説明する。他のチャンネル参加者には内容が見えず、本人以外が押しても内容を返さない。利用者はこの説明を確認し、個人情報等に対する一段階の確認操作を了承した。DMへ移動する新導線は導入しない。

MFR-13：長文は意味と順序を保持して分割し、必要な全ページを表示できるまで許可を出さない。拒否は途中でも行える。個人情報の検査規則・範囲・限界を[判定仕様](mcp-approval-privacy-design.ja.md)と利用者向け説明に明記し、定義した検査例を全ての承認経路で確認する。

MFR-14：0.5の新scope・policy／定義世代・許可条件を操作詳細、受理時メタデータ、許可一覧に明示し、Gatewayが全項目を保存・照合できる契約を提供する。現在のボタン判定は最新presentationを正本とする。

MFR-15：正常なcatalog定期更新だけで利用者に再承認を連発しない。有効な既存grantの後続callを有界に待機し、同一定義更新後にProxyが全条件を再評価する。失敗・停止・取消・期限切れ・世代変更を正常更新と区別し、失効済みgrantを復活させない。F12〜F14で検証する。

MFR-16（2026-09-17利用者決定）：明示的に該当ポリシーを指定した実行では、変更対象を保証できないNotion部分置換を実行しない。単発確認でも回避せず、理由を説明する。現導入ツールで実行時の本文版を保証できないupdate_contentはunavailableとする。暗黙の別操作への変換は禁止。この明示制限を前提として内部設計を確定し、将来の上流保証待ちを現版の実装開始条件にしない。

### 汎用基盤と追加ポリシーの責務確定（2026-09-17）

本節と[責務再整理](mcp-approval-layering-revision.ja.md)は、上記の全ツール意味評価を汎用承認の必須条件にした記述を改訂する。既存APIのwire変更は後継契約のレビューで確定する。

MFR-17：Proxy中核はCodex実行、実call・全引数、承認対応・単発返信、停止、状態照会、永続化・復旧を保証する。Discordの本人確認・ボタン・投稿分割はクライアント責務。中核が特定クライアント名を見て承認方式を変えることを禁止する。

MFR-18：実callと完全な実引数を確認できる場合、意味解析未対応だけを理由に上流の単発承認を禁止しない。公開範囲の判定・本人向け表示・欠落防止・全ページ照合は維持し、安全性評価済みという表示をしない。型付きの原引数を保持し、未知キーを捨てない。

MFR-19：依頼中許可は独立した汎用ポリシー拡張とする。登録済みポリシーの明示選択・適用結果を実行へ拘束し、未評価操作を自動許可しない。表示profileの選択、機能ON、単発Allowをポリシー選択・包括許可と混同しない。

MFR-20（利用者決定）：Notion部分置換禁止は該当ポリシーを指定した実行だけに適用する。標準APIや他クライアントへ暗黙適用しない。適用中は古い画面・別profile・直接reply・既存grantでも回避不可とする。未選択実行と上流の承認設定を共有して制限を漏らさない。

MFR-21：全342ツールの完全な意味解析を汎用承認改善の実装開始条件としない。中核・汎用表示・ポリシー機構の詳細設計と接続契約を先に完成し、評価済み範囲を明示して実装する。未評価操作の単発経路を含む製品全体の受入を必須とし、3形式のみの表示成功を全体完成としない。

MFR-17〜21の具体接続契約は[API 0.6接続合意版](mcp-approval-api-v06.ja.md)。profile=source-conversation-v3で表示とpolicy選択を分離し、未選択・未評価の単発、選択中の禁止、実効設定の照会と次Runへの非継承を定義。接続Goを受領し、[機能単位の内部設計](mcp-approval-v06-implementation.ja.md)が完成した単位から実装へ進む。

MFR-22（API 0.6レビュー補完）：応答上限をエンドポイントごとに公開し、policy・Response・interaction・grantの発行前に将来状態込みの容量を予約する。発行済みgrantを一覧不能にして取消導線を失わせず、停止／取消を一覧取得から独立させる。

MFR-23：policy準備の期限は永続受付から60秒。停止をTurn ID不要で受理し、ready確定・Turn送信意思と直列化する。設定の結果不明とAI開始の結果不明を分け、準備再起動は同じResponse/bindingと元期限で照合する。未送信・設定隔離を証明するまで次Runの占有を解除しない。
