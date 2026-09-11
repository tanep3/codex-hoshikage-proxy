# OpenWebUI登録ガイド

**日本語** | [English](openwebui.md)

このガイドのAPI参照版はOpenWebUI `v0.11.0`です。このサーバーでは2026-09-11に`0.6.36`から`0.11.3`へ更新され、標準Manifold Pipe `0.5.1`を反映しました。登録先の実際の版と設定を確認してください。

## 1. OpenWebUIからProxyへ到達できるようにする

OpenWebUIがDockerで動き、Proxyがホストで動く場合、OpenWebUIから `127.0.0.1` を指定してはいけません。
コンテナ内の `127.0.0.1` はコンテナ自身です。例えば次のようにホストの到達可能なアドレスを使います。

```text
http://192.168.0.120:4040
```

Pipeへ設定するURLは `/v1` を付けないProxyのベースURLです。Pipeが `/v1/models` とAPIパスを追加します。

## 2. ProxyのAPI Keyを設定する

現在のProxyではloopbackを含めAPIキーが必要です。Proxy設定へキーを記述します。

```toml
[security]
api_key = "YOUR_PROXY_API_KEY"
```

実運用では長くランダムな値を使ってください。この同じ値をOpenWebUIのPipe設定 `PROXY_API_KEY` へ入力します。
`api_key_env` を使う場合は、Proxyプロセスの環境変数へ設定します。

## 3. Pipeを登録して設定する

1. OpenWebUIの管理画面からFunctions/Pipes管理画面を開く。
2. 付属の `openwebui/codex_hoshikage_pipe.py` をPipeとして作成またはインポート。
3. Pipeの設定／Valvesを開く。
4. 次を設定。

   ```text
   PROXY_BASE_URL = http://192.168.0.120:4040
   PROXY_API_KEY = YOUR_PROXY_API_KEY
   REQUEST_TIMEOUT_SECONDS = 120
   HEALTHCHECK_TIMEOUT_SECONDS = 2
   REASONING_EFFORT = low
   ```

   アドレスとキーは自分の値へ置き換えます。
   ChatGPTモデルの推論レベルを変える場合は、`REASONING_EFFORT` を `low`、`medium`、`high` のいずれかに変更します。
   HoshikageとOllamaモデルでは、各プロバイダのデフォルト設定を使います。
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

Pipeが古いResponse IDを保持したままProxyを再起動すると、Codex側のThreadが存在しなくなることがあります。
Pipeは`thread_not_found`を検知すると古いIDを破棄し、OpenWebUIが持つ表示中の会話履歴から新しいThreadを作り直して1回だけ再試行します。

## 5. 承認の動き

PipeはOpenWebUI標準の `__event_call__` 承認イベントを使います。Proxyのドメインは `accept`、`accept_for_session`、
`decline`、`cancel` の4値を扱えますが、標準UIは現在2つのボタンしか表示しません。Pipeは2ボタンの操作を対応するCodex判断へ変換してProxyへ渡します。

承認そのものはCodexとProxyのサーバー側で強制されます。2ボタンUIだけを安全策とみなさないでください。

承認タイムアウト時、Proxyは要求を期限切れにして後始末します。しかしOpenWebUI標準イベントAPIにはサーバーから確実にダイアログを閉じる機能がないため、
画面上のダイアログが残ることがあります。後から押した操作は古い要求として拒否されます。これは標準Pipeの既知の制約であり、UIイベント経路が明示的な閉鎖に対応するまで運用上の制約です。

承認中にリロードまたは切断した場合、Pipeの切断処理によってCodex Turnをキャンセルします。手動キャンセルでもTurnが終了し、Provider permitが解放されます。

### PIPE 0.5.1の承認修正

承認中は対象Turnの未処理一覧を0.5秒間隔で照合し、複数の要求を順に表示します。ダイアログには対象操作の詳細を表示し、送信時にTurn/Threadを照合します。期限切れ・他のクライアントによる解決を確認した要求は送信しません。監視通信や確認イベントが失敗した場合は、生成を無言で待たせずエラーを表示し、v1の接続を終了します。結果不明の承認判断は自動再送しません。

回帰試験は`python -m unittest discover -s tests -p "test_openwebui_pipe.py"`（httpx・pydanticが必要）。`cargo build --bins`後は実Proxyと模擬App Serverを使ったHTTP承認往復も実行します。ブラウザでの実ダイアログ確認を代替するものではありません。

## 6. 困ったとき

- **NetworkProblem**: OpenWebUIコンテナからURLへ到達できるか確認。`127.0.0.1` ではなくホストのLAN IPを使い、4040番ポートが待受中か確認。
- **401**: PipeのキーとProxyの `security.api_key` が完全に一致しているか確認。
- **`/v1/chat/completions` が404**: 古いProxyまたは違うポートを見ています。現在のProxyを再起動し、`/v1`なしのベースURLを設定。
- **モデルが一部しか出ない**: Pipeを更新し、プロバイダ有効化とモデル登録を確認。ツール呼び出し非対応のHoshikageモデルは意図的に除外されます。
- **`tool_calling_not_supported`**: プロバイダ一覧でツール対応と報告されるモデルを選んでください。
- **Proxy再起動後の`thread_not_found`**: PipeがOpenWebUIの会話履歴から自動復旧します。それでも失敗する場合は、Pipeを一度再読み込みしてメモリ上の対応表を消してください。

## 7. Gateway拡張API v2との関係

PIPE 0.5.1はOpenAI互換`/v1/responses`と制御API v1を使用します。v2が有効になっただけでは、PIPEの機能は切り替わりません。v2を利用すれば、会話IDでの継続、確定回答の再取得、成果物の一覧・取得、Turn開始前の停止を実装できます。
移行にはOpenWebUIのユーザー・チャット・分岐とProxy会話の対応、要求キーの永続化、明示停止と通信切断の区別、成果物の認可・表示をPIPE側で設計する必要があります。現在の固定論理会話IDを用いた文脈共有実験とは分けて扱います。

## 8. このサーバーへの反映記録（2026-09-11）

OpenWebUI `0.11.3`の登録済み`codex_hoshikage_proxy`をPIPE `0.5.1`へ更新しました。接続先は`http://192.168.0.120:4040`、APIキーはValvesに設定し、ソースへ埋め込みません。旧登録内容・設定は`/home/tane/tools/docker/open-webui/pipe-backup-20260911T130638Z/`へ権限を制限して保存しています。

管理用ブラウザ接続を利用できないため、対象Function行のみをSQLiteトランザクションで更新しました。更新前の内容一致を検査し、名前・所有者・有効化状態を維持しています。OpenWebUIのローダーがDBのソース差分を検出して再読み込みすることをインストール済みソースで確認しました。本体再起動は行っていません。

検証はローカル5件（実Proxy＋模擬App ServerのHTTP承認往復を含む）、コンテナ内の登録済みコードで承認回帰4件を通過しました。さらに登録済みコードからモデル一覧6件・実Codex生成`PIPE_DEPLOY_OK`を確認しました。ブラウザ上のダイアログ表示は未確認です。

## PIPE 0.6.0の画像対応

テキストと画像を含むメッセージを維持し、OpenWebUIへアップロードされた画像は標準のファイルアクセス権を検証してdata URLへ変換する。1画像10 MiB、入力合計15 MiBまで。読み込み失敗を無視してテキストだけで実行しない。

Codex画像生成ツールが作成したPNGは、完了したResponseの生成画像APIから取得し、OpenWebUIの利用者所有ファイルとして保存してMarkdown画像で表示する。ProxyのAPIキーを画像URLへ埋め込まない。画像生成ツール以外がワークスペースへ作った任意ファイルの自動回収や、v2の汎用成果物メニューへの移行は含まない。
