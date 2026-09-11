# このサーバーの常駐設定

設定日: 2026-09-11。ユーザー`tane`のsystemdサービスとして稼働する。

| 項目 | 設定 |
| --- | --- |
| サービス | `codex-hoshikage-proxy.service`（ユーザーサービス） |
| 待ち受け | `0.0.0.0:4040` |
| このサーバーのLAN URL | `http://192.168.0.120:4040` |
| OpenAI互換クライアントのBase URL | `http://192.168.0.120:4040/v1` |
| APIキー | `~/.config/codex-hoshikage-proxy/api-key`（権限600） |
| サービス環境変数 | `~/.config/codex-hoshikage-proxy/environment`（権限600） |
| 設定 | `~/.config/codex-hoshikage-proxy/config.toml` |
| バイナリ | `~/.cargo/bin/codex-hoshikage-proxy`（releaseビルド） |
| Codexホーム | `~/.config/codex-hoshikage-proxy/codex-home` |
| 専用作業ディレクトリ・cwd許可範囲 | `/home/tane/work/codex-hoshikage` |
| 初期有効プロバイダ | ChatGPT（Hoshikageは未設定のため無効） |
| デフォルトモデル | `chatgpt/gpt-5.6-luna` |
| Sandbox | workspace-write、sandboxからのnetwork_access=false |
| 自動承認 | false。承認要求には対話承認対応クライアントが必要 |
| 自動起動 | enabled、ユーザーのLinger=yes |
| 自動復旧 | App Server異常終了 → Proxy非ゼロ終了 → systemdが5秒後に再起動 |

APIキーは本文やリポジトリに記載しない。クライアントは`Authorization: Bearer <APIキー>`を送る。
`/healthz`と`/readyz`もキーが必要。LANアドレスは固定設定したものではなく、変更された場合はクライアントの接続先を更新する。

## 操作

```sh
systemctl --user status codex-hoshikage-proxy --no-pager
journalctl --user -u codex-hoshikage-proxy -n 100 --no-pager
systemctl --user restart codex-hoshikage-proxy
systemctl --user stop codex-hoshikage-proxy
```

ローカルで認証付きの確認をする例:

```sh
source ~/.config/codex-hoshikage-proxy/environment
curl -H "Authorization: Bearer ${PROXY_API_KEY}" http://127.0.0.1:4040/readyz
```

設定を編集した場合はサービスを再起動する。unitを編集した場合は先に`systemctl --user daemon-reload`も実行する。
60秒に6回の起動制限に達した場合は、ログで原因を修正した後に以下を実行する。

```sh
systemctl --user reset-failed codex-hoshikage-proxy
systemctl --user start codex-hoshikage-proxy
```

## 復旧の動作範囲

実行中だった要求は自動再送しない。クライアントにはエラーとして伝える。
保存済みResponses会話の次の要求は`previous_response_id`から対応するThreadを`thread/resume`で再読み込みする。
進行中の承認状態はメモリ上にあり、再起動で失われる。明示的な停止はSIGTERMで正常終了する。
HTTP終了待ちの上限は5秒、サービスの停止上限は15秒とする。

サーバー自身からLAN IPで認証・モデル一覧（6件）・生成を検証済み。App Serverを強制終了すると約5.34秒で自動復旧し、再起動前のprevious_response_idで会話継続できた。検証時のsystemd再起動回数は1。別のLAN PCからの到達性と、OS再起動後の自動起動は設定確認までで、実機操作による確認は未実施。

## バイナリ更新

`cargo build --release --bin codex-hoshikage-proxy`でビルドし、既存バイナリを退避してから同じディレクトリ内の一時ファイルから原子的に置換する。進行中の実行・接続を確認し、`systemctl --user restart codex-hoshikage-proxy`で反映する。設定ファイルやAPIキーを上書きしない。

更新後は認証付き`/readyz`、`/v1/codex/capabilities`（契約1.0）、モデル一覧、短いResponses生成と要求ID・Turn状態照会を確認する。認証なしが401になること、`0.0.0.0:4040`の待受も確認する。v2移行後は古いバイナリだけへの切戻しを行わない。旧版はv2の占有・停止記録を認識しないため、[v2復旧手順](v2-operations.ja.md)に従う。v2要求を一度も受理していない移行失敗時に限り、停止中に取得した移行前バックアップの設定・状態・バイナリを一組で復元する。

v1のみの旧版では実行メタデータを`state/responses/executions.jsonl`へ同期保存する。新形式の記録と旧`mappings.jsonl`を維持し、更新・ロールバック時に状態ディレクトリを削除しない。

## 2026-09-11 制御API v1適用記録

09:03 JSTに実装コミット`6875bb0859878d3ac7f3d4dcd185e1db88c05244`のreleaseビルドを適用した。
適用記録は`~/.config/codex-hoshikage-proxy/last-update.json`、旧バイナリは
`~/.cargo/bin/codex-hoshikage-proxy.before-6875bb085987`に保存した。

反映後にサービス稼働・自動起動有効・`0.0.0.0:4040`待受を確認した。
サーバー自身からLAN IPへ接続し、キーなし／不正キーの401、制御API契約1.0、モデル一覧6件を確認した。
実Codexへの短いResponses要求は`DEPLOY_OK`で完了し、要求ID照会・Turnのcompleted状態・同一Idempotency-Key再送時の同一Response IDも確認した。
これはProxyの受入記録であり、これから実装するGatewayや別LAN PCからの接続を検証したものではない。

## v2を標準提供する版への移行

OpenAI互換`/v1`とGateway拡張`/v2/codex`を同時に提供する。通常は`server.v2_enabled`の設定不要。既存のAPIキー・LAN待受・承認方針を継承する。更新時にはサービス停止中のホームと旧バイナリを退避する。
初回起動で旧Responses台帳を`state/v2/metadata.sqlite3`へ移行し、移行元を`state/responses/pre-v2/`に保存する。v2の状態がある環境では無効化による起動を拒否する。以降のバックアップ・復元には[v2運用手順](v2-operations.ja.md)を使用する。
