# Codex Native API 契約

2026-09-23 / 1.0。`/v2/codex`を廃止し、Codex App ServerのJSON-RPCを忠実にネットワーク公開する`/codex`を定義する。

## 1. 目的

Proxyは次の二つの公開面を持つ。

| 公開面 | 役割 |
| --- | --- |
| `/v1/*` | ChatGPT、Hoshikage、OllamaをOpenAI互換APIとして利用する |
| `/codex` | Codex App ServerのJSON-RPC接続をWebSocketへ移送する |

`/codex`はGatewayのAPIではない。Discord、チャンネル、承認カード、意味ベース委任、特定MCPツールの判断を持たない。

## 2. 接続契約

クライアントは`GET /codex`をWebSocketへUpgradeする。Upgrade要求には通常APIと同じ`Authorization: Bearer <API key>`を付ける。API keyをURL、query、WebSocket subprotocolへ入れない。

接続を受理すると、Proxyはその接続専用のCodex App Serverを`stdio`で起動する。WebSocketのテキストメッセージ一件とApp Serverの改行区切りJSON一件を、一対一で双方向転送する。

- クライアントはCodex App Server仕様どおり、最初に`initialize`を送り、成功後に`initialized`通知を送る。
- ProxyはJSON-RPCの`method`、`id`、`params`、`result`、`error`を解釈して別契約へ変換しない。
- App Serverからの通知とserver requestも同じ接続へ転送する。承認や入力要求への応答はクライアントがJSON-RPCで返す。
- 未知のメソッド、未知の通知、未知のフィールドを、Proxyが未実装という理由で削除または拒否しない。
- 一つのWebSocket接続を一つのApp Server接続へ対応させる。異なるクライアント間でJSON-RPC ID、承認、設定、プロセス内状態を共有しない。

バイナリWebSocketメッセージは受理しない。UTF-8でないメッセージ、JSON値でないメッセージ、一メッセージに複数のJSON値を含む入力は、接続エラーとして閉じる。

## 3. ライフサイクル

WebSocket切断時は、当該接続専用App Serverのstdinを閉じ、終了猶予後も残る場合は停止する。Proxyは未完了のJSON-RPC要求を別プロセスへ自動再送しない。Codexが永続化したThreadは、新しい接続から`thread/resume`等の正式APIで復元する。

App Serverが終了した場合、ProxyはWebSocketを内部エラーで閉じる。接続中にApp Serverを差し替えて、古いJSON-RPC IDや承認待ちを継続したように見せない。

Proxy全体のsystemd再起動と、接続専用App Serverの終了を区別する。`/v1`用のApp Serverと`/codex`接続用App Serverは、相互の実行・承認状態を共有しない。

## 4. 認証と秘密情報

- 非loopback待受ではAPI keyを必須とする。
- `/v1/*`、`/codex`、管理用の保護対象APIは同じBearer認証規則を使う。
- `/healthz`と`/readyz`の公開可否は設定で一貫させる。認証失敗を準備未完了や上流障害として返さない。
- API keyはログ、エラー本文、URL、メトリクスへ出さない。
- 比較は入力長や先頭一致に依存しない方法を使う。
- ProxyはCodexの`auth.json`をクライアントへ返さない。Codex App ServerのJSON-RPC応答に含まれないローカル資格情報を追加公開しない。

単一API keyはProxy利用者全体を同じ信頼主体として扱う。利用者別の認可境界を装わない。複数主体が必要になった場合は、API keyごとの権限を別要件として設計する。

## 5. 制限と背圧

`/codex`は標準機能として常時提供し、有効化スイッチを設けない。初期実装の既定値は次のとおりとする。

| 項目 | 既定値 |
| --- | ---: |
| WebSocket同時接続数 | 16 |
| 双方向の一メッセージ最大bytes | 8 MiB |
| App Server終了猶予 | 5秒 |
| 各方向で書込み待ちにできるメッセージ | 1件 |

上限超過を切り捨てたJSONとして転送しない。明確なエラーで接続を閉じる。独自の蓄積queueを持たず、各方向の書込み完了を待ってから次を処理し、遅いクライアントのために無制限のメモリを保持しない。

Upgrade前の認証失敗は401、接続数上限は503とする。Upgrade後は、バイナリ入力を1003、UTF-8またはJSON不正を1007、サイズ超過を1009、App Server起動・異常終了・stdio障害を1011で閉じる。正常なクライアント切断は、そのcodeを可能な範囲で尊重して子プロセス終了へ進む。

設定値は起動時に検証し、0、範囲外、内部buffer上限を超える値で起動しない。設定名と既定値は[システム設計書](codex-hoshikage-proxy-system-design.md)を正本とする。

## 6. Proxyが判断しない事項

Proxyは`/codex`に対して次を行わない。

- ツール名やMCPサーバー名による承認適格性の判定
- 操作の意味、安全性、利用者の目的、委任範囲の判定
- Codexの承認選択肢を独自の許可種別へ変換
- Gateway、Discord、OpenWebUI固有のIDや表示文言の保存
- 特定サービスの操作禁止規則の追加
- クライアントの代わりにserver requestへ回答

Codex自身の設定、sandbox、approval policy、permission profileは、Codex App Serverの正式な要求と設定を通じてクライアントが選択する。Proxyのホスト運用者が強制する設定は、Proxy設定として明示し、クライアント要求を受理してから黙って置換しない。

## 7. `/v2/codex`の廃止

旧`/v2/codex`はGateway専用として作られ、現在のGatewayは使用していないため削除する。旧DBの会話、成果物、リース、interaction、presentation、grant、意味評価、Notion guardは新APIへ移行しない。

削除前に、OpenAI互換`/v1`が利用している画像取得、ResponseとThreadの対応、承認、制御APIの実装依存を切り分ける。V1で利用中の機能を、旧V2の削除とともに失わない。

## 8. 受入条件

1. WebSocket経由で`initialize`、`model/list`、`thread/start`、`turn/start`を実行できる。
2. 通知、文字列ID、数値ID、`null` resultを損失なく双方向転送できる。
3. App Serverからのserver requestを転送し、同じIDのクライアント応答が上流へ届く。
4. Proxyが知らないmethodと追加フィールドを上流へ渡せる。
5. 二接続の同じJSON-RPC IDが衝突せず、承認・設定・終了が混ざらない。
6. 切断、App Server異常終了、Proxy停止で未完了要求を自動再送しない。
7. API keyの欠落・不一致を401として区別し、秘密を応答やログへ出さない。
8. サイズ・queue・接続数上限で有界に動作する。
9. `/v1`のOpenAI互換APIとOpenWebUI Pipeの回帰試験が成功する。
10. 旧`/v2/codex`が404となり、旧V2設定なしで起動できる。

