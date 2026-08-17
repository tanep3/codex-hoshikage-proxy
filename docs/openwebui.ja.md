# OpenWebUI登録ガイド

**日本語** | [English](openwebui.md)

このガイドはOpenWebUI `v0.11.0` と付属の標準Manifold Pipeを対象にします。

## 1. OpenWebUIからProxyへ到達できるようにする

OpenWebUIがDockerで動き、Proxyがホストで動く場合、OpenWebUIから `127.0.0.1` を指定してはいけません。
コンテナ内の `127.0.0.1` はコンテナ自身です。例えば次のようにホストの到達可能なアドレスを使います。

```text
http://192.168.0.220:4040
```

Pipeへ設定するURLは `/v1` を付けないProxyのベースURLです。Pipeが `/v1/models` とAPIパスを追加します。

## 2. ProxyのAPI Keyを設定する

非loopback待受ではProxy設定へキーを記述します。

```toml
[security]
api_key = "tane-codex-proxy-local-key"
```

実運用では長くランダムな値を使ってください。この同じ値をOpenWebUIのPipe設定 `PROXY_API_KEY` へ入力します。
`api_key_env` を使う場合は、Proxyプロセスの環境変数へ設定します。

## 3. Pipeを登録して設定する

1. OpenWebUIの管理画面からFunctions/Pipes管理画面を開く。
2. 付属の `openwebui/codex_hoshikage_pipe.py` をPipeとして作成またはインポート。
3. Pipeの設定／Valvesを開く。
4. 次を設定。

   ```text
   PROXY_BASE_URL = http://192.168.0.220:4040
   PROXY_API_KEY = tane-codex-proxy-local-key
   REQUEST_TIMEOUT_SECONDS = 120
   HEALTHCHECK_TIMEOUT_SECONDS = 2
   ```

   アドレスとキーは自分の値へ置き換えます。
5. 保存し、PipeのManifoldモデルを有効化します。

通常のリクエスト前に、PipeはProxyの`/readyz`を確認します。Proxy停止中、またはCodexの準備ができていない場合は、
長いリクエストタイムアウトまで待たずに処理を終了し、状態を表示します。別モデルへ勝手に送ることはしません。

Pipeは `/v1/models` を取得し、モデルを `Codex / provider / provider/model` のように表示します。
設定変更後はPipeのモデル一覧を更新してください。

## 4. 文脈継承の実験

現在のPipeは内部でProxyのResponses APIを呼び出します。デフォルトの論理会話IDは次の値です。

```text
openwebui_id_001
```

これはPipe側の会話キーであり、CodexのThread IDではありません。内部ではOpenWebUIのユーザーIDを組み合わせて分離します。
Pipeプロセスが生きている間は、同じユーザーのスレッドで同じResponsesの文脈を共有し、別ユーザーとは共有しません。

選択モデルを変更すると、Pipeは新しいCodex Threadを開始し、OpenWebUIから渡された会話履歴を新モデルへ渡します。
論理会話IDは同じなので、モデル変更後も会話を続けられます。現在は対応表をメモリ上に保持する初期実験のため、OpenWebUIがPipeを再読み込みすると失われます。

## 5. 承認の動き

PipeはOpenWebUI標準の `__event_call__` 承認イベントを使います。Proxyのドメインは `accept`、`accept_for_session`、
`decline`、`cancel` の4値を扱えますが、標準UIは現在2つのボタンしか表示しません。Pipeは2ボタンの操作を対応するCodex判断へ変換してProxyへ渡します。

承認そのものはCodexとProxyのサーバー側で強制されます。2ボタンUIだけを安全策とみなさないでください。

承認タイムアウト時、Proxyは要求を期限切れにして後始末します。しかしOpenWebUI標準イベントAPIにはサーバーから確実にダイアログを閉じる機能がないため、
画面上のダイアログが残ることがあります。後から押した操作は古い要求として拒否されます。これは標準Pipeの既知の制約であり、UIイベント経路が明示的な閉鎖に対応するまで運用上の制約です。

承認中にリロードまたは切断した場合、Pipeの切断処理によってCodex Turnをキャンセルします。手動キャンセルでもTurnが終了し、Provider permitが解放されます。

## 6. 困ったとき

- **NetworkProblem**: OpenWebUIコンテナからURLへ到達できるか確認。`127.0.0.1` ではなくホストのLAN IPを使い、4040番ポートが待受中か確認。
- **401**: PipeのキーとProxyの `security.api_key` が完全に一致しているか確認。
- **`/v1/chat/completions` が404**: 古いProxyまたは違うポートを見ています。現在のProxyを再起動し、`/v1`なしのベースURLを設定。
- **モデルが一部しか出ない**: Pipeを更新し、プロバイダ有効化とモデル登録を確認。ツール呼び出し非対応のHoshikageモデルは意図的に除外されます。
- **`tool_calling_not_supported`**: プロバイダ一覧でツール対応と報告されるモデルを選んでください。
