# OpenWebUI Pipe 0.7 設計

2026-09-23。Codex Native APIへの構造改定と、認証状態の誤表示修正を扱う。

## 1. 適用範囲

OpenWebUI Pipeは引き続きOpenAI互換の`/v1`を利用する。`/codex`はCodex App ServerのJSON-RPCを直接扱うクライアント向けであり、Pipeが移行する先ではない。

Pipeは次の二つの認証を混同しない。

| 境界 | 資格情報 | 失敗時の意味 |
| --- | --- | --- |
| OpenWebUI Pipe → Proxy | Proxy API key | Proxyへの接続を許可されていない |
| Proxy → Codex / ChatGPT | Codexのログイン状態 | ChatGPT providerを現在実行できない |

OpenWebUIのAuthorizationヘッダー、Cookie、利用者tokenをProxyへ転送しない。

## 2. 調査で確認した現象

2026-09-23の常駐環境では、Pipe設定のAPI keyとProxyの実効API keyは一致しており、正しいkeyの`GET /v1/models`は200、不正keyとkeyなしは401だった。

一方、Codex App Serverは`token_revoked`を伴う401を記録していたが、Proxyは過去に取得したChatGPTモデルを`available`として返していた。このため、利用者にはモデルを選択できるように見え、実行時に初めて認証失敗が現れる。

ログ文字列の解析は製品実装に使わない。Codex App Serverの正式なaccount RPC、認証更新通知、要求失敗を用いてprovider状態を更新する。

## 3. 要求

1. `PROXY_API_KEY`はOpenWebUIの秘密型で保持し、画面・ログ・例外・URLへ出さない。空値を許すのはProxyがloopback限定で認証なしを明示設定している場合だけとする。
2. Proxyから401を受けた場合は「Proxy APIキーが未設定または一致しません」と表示し、Codexログイン失敗と混同しない。
3. Proxyはproviderごとに`available`、`authentication_required`、`temporarily_unavailable`を返す。状態の根拠と最終確認時刻を機械可読にする。
4. 認証失効を確認したChatGPT providerのモデルを、実行可能モデルとして`/v1/models`へ残さない。古い成功キャッシュだけで復活させない。
5. 実行開始後に認証失効が確定した場合は、`provider_authentication_required`として返す。一般的なProxy未準備やネットワーク失敗へ丸めない。
6. ChatGPT認証失効だけでProxy全体の`/readyz`を失敗させない。HoshikageやOllamaなど、実行可能なproviderがあれば全体は稼働を継続できる。
7. Pipeの`pipes()`は一覧取得失敗を空一覧へ黙って変換せず、安全な診断ログを残す。会話実行では利用者に原因別メッセージを返す。
8. PipeはOpenWebUIのAuthorization、Cookie、Discord等の外部認証情報をProxyへ転送しない。
9. 秘密値をモデル名、metadata、画像URL、エラー本文へ含めない。

## 4. Provider状態API

`GET /v1/codex/capabilities`のprovider情報を、次の形へ拡張する。OpenAI互換の`/v1/models`には独自フィールドを混入させない。

```json
{
  "providers": {
    "chatgpt": {
      "status": "authentication_required",
      "reason": "token_revoked",
      "checked_at": "2026-09-23T12:00:00Z"
    },
    "hoshikage": {
      "status": "available",
      "reason": null,
      "checked_at": "2026-09-23T12:00:00Z"
    }
  }
}
```

`reason`は公開可能な固定enumとし、上流応答本文やtokenをそのまま返さない。状態が未確認の場合は`temporarily_unavailable`とし、古い`available`を無期限に使わない。

## 5. 状態更新

- 起動時およびApp Server世代変更時にaccount状態を正式RPCで確認する。
- `account/updated`、認証回復開始・完了通知、実行時の正式な認証エラーで状態を更新する。
- `model/list`がキャッシュを返した事実だけを、認証成功の証拠にしない。
- provider状態の更新失敗と、認証が失効した事実を区別する。
- 認証回復後はモデル一覧を再取得してから`available`へ戻す。

## 6. 利用者向け表示

| 状態 | 表示例 |
| --- | --- |
| Proxy API key不一致 | Proxy APIキーが未設定または一致しません。OpenWebUIのPipe設定を確認してください。 |
| Codex認証切れ | Codexのログインが失効しています。Proxyを実行している環境でCodexへ再ログインしてください。 |
| 一時障害 | Codexの状態を確認できません。しばらくしてから再試行してください。 |
| Proxy停止 | Proxyへ接続できません。常駐サービスの状態を確認してください。 |

利用者向け表示にHTTP応答全文、内部パス、token、API keyを含めない。詳細はProxyとOpenWebUIの安全な運用ログへ残す。

## 7. 受入条件

1. 正しい／不正／未設定のProxy API keyを区別できる。
2. Codexの認証失効をProxy API key不一致と表示しない。
3. 認証失効済みChatGPTモデルを実行可能一覧に表示しない。
4. ChatGPT認証失効中も、利用可能な別providerは選択・実行できる。
5. 再ログイン後、正式な状態確認とモデル再取得を経てChatGPTモデルが復帰する。
6. `pipes()`の失敗、会話実行の失敗、画像取得の失敗を区別して記録する。
7. ログ、エラー、OpenWebUI設定表示に秘密値が現れない。
8. `/v1`の会話継続、承認、画像入出力に回帰がない。

