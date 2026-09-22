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

現行版はOpenAI互換`/v1`とCodex Native API `/codex`を提供する。`/codex`はWebSocket一接続ごとに専用Codex App Serverをstdioで起動する。旧Gateway専用`/v2/codex`と`[v2]`設定は廃止済みである。旧`state/v2/metadata.sqlite3`は自動削除せず、V1の継続用記録だけを初回起動時に`state/responses/control.sqlite3`へ読取移行する。

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

更新後は認証付き`/readyz`、`/v1/codex/capabilities`、モデル一覧、短いResponses生成と要求ID・Turn状態照会を確認する。認証なしが401になること、`/codex`で`initialize`できること、旧`/v2/codex`が404になること、`0.0.0.0:4040`の待受も確認する。状態を戻す場合はバイナリ、設定、`state/responses`を同じ停止時点の一組として扱う。旧V2 DBは現行版の書込み先ではない。

v1のみの旧版では実行メタデータを`state/responses/executions.jsonl`へ同期保存する。新形式の記録と旧`mappings.jsonl`を維持し、更新・ロールバック時に状態ディレクトリを削除しない。

## 2026-09-11 制御API v1適用記録

09:03 JSTに実装コミット`6875bb0859878d3ac7f3d4dcd185e1db88c05244`のreleaseビルドを適用した。
適用記録は`~/.config/codex-hoshikage-proxy/last-update.json`、旧バイナリは
`~/.cargo/bin/codex-hoshikage-proxy.before-6875bb085987`に保存した。

反映後にサービス稼働・自動起動有効・`0.0.0.0:4040`待受を確認した。
サーバー自身からLAN IPへ接続し、キーなし／不正キーの401、制御API契約1.0、モデル一覧6件を確認した。
実Codexへの短いResponses要求は`DEPLOY_OK`で完了し、要求ID照会・Turnのcompleted状態・同一Idempotency-Key再送時の同一Response IDも確認した。
これはProxyの受入記録であり、これから実装するGatewayや別LAN PCからの接続を検証したものではない。

## 履歴：v2を標準提供した版への移行

ここから下のV2節は過去の配備記録であり、現行運用手順ではない。

OpenAI互換`/v1`とGateway拡張`/v2/codex`を同時に提供する。通常は`server.v2_enabled`の設定不要。既存のAPIキー・LAN待受・承認方針を継承する。更新時にはサービス停止中のホームと旧バイナリを退避する。
初回起動で旧Responses台帳を`state/v2/metadata.sqlite3`へ移行し、移行元を`state/responses/pre-v2/`に保存する。v2の状態がある環境では無効化による起動を拒否する。以降のバックアップ・復元には[v2運用手順](v2-operations.ja.md)を使用する。

## 2026-09-11 Gateway拡張API適用記録

21:05 JSTに実装コミット`81783d1217a6576ac698dbdd694c40ba555f5c6d`のreleaseビルドを適用した。
OpenAI互換APIとGateway拡張API v2を同時に標準提供する。既存の設定ファイル・APIキー・承認設定・`0.0.0.0:4040`待受を継承し、バージョン切替設定は追加していない。
停止直前に進行中・UNKNOWNの実行と既存HTTP接続がないことを確認し、停止中のProxyホームと旧バイナリを`~/.config/codex-hoshikage-proxy.before-v2-20260911T120528Z/`へ退避した（ディレクトリ権限700）。適用記録は`~/.config/codex-hoshikage-proxy/last-update.json`。

初回起動で旧実行・会話台帳をSQLiteへ移行し、元のJSONLを`state/responses/pre-v2/`に保存した。
反映後に稼働・自動起動有効・LAN待受と、サーバー自身からLAN IP経由で以下を確認した。

- 認証なしの401、認証付きreadiness、モデル一覧6件、制御API v1と拡張API v2のcapabilities。
- 実Codexで`/v1/responses`が`DEPLOY_V1_OK`、`/v1/chat/completions`が`DEPLOY_CHAT_OK`を返すこと。
- v2会話作成・非同期実行・確定回答取得で`DEPLOY_V2_OK`、実行状態がcompletedになること。
- v1/v2とも同じ要求キーの再送が同じResponse IDを指し、新規実行しないこと。v1の重複応答は契約どおり実行記録を返す（検証スクリプトの`id`参照を`response_id`へ修正して照合）。

標準有効化後の自動テスト106件、Clippy警告ゼロ、整形・差分チェックを通過した。別LAN PC・Gateway/Discord結合・混合負荷の未確認項目は[v2受入記録](v2-implementation-status.ja.md)に残す。

## 2026-09-11 画像対応の反映記録

22:40 JSTに`a6eded43651381ecbbe6dd55a9762dbfca1bc458`のreleaseバイナリへ更新。設定・認証・LAN待受を維持し、旧版を`~/.cargo/bin/codex-hoshikage-proxy.before-images-20260911T134017Z`へ退避した。Gatewayは常駐したまま正常に再接続している。

Proxyテスト107件とClippy、PIPEテスト8件を通過。実画像入力による識別、生成画像APIの元PNGとのバイト一致、OpenWebUI利用者所有ファイルへの保存・画像リンク生成を確認。PIPE 0.6.0を登録済み。旧登録内容は`/home/tane/tools/docker/open-webui/pipe-backup-20260911T134107Z/`、復旧した作画回答の変更前内容は`/home/tane/tools/docker/open-webui/image-chat-backup-20260911T134257Z/`に権限を制限して保存している。

## 2026-09-12 v2生成画像対応の反映記録

21:23 JSTに実装コミット `09b75a9` を常駐サービスへ反映した。`cargo build --locked --release --bin codex-hoshikage-proxy` のバイナリを退避付きで原子的に置換した。更新直前と停止後に実行中・UNKNOWNの依頼、保存中の成果物がないことを確認。設定・APIキーは変更していない。

旧バイナリは `~/.cargo/bin/codex-hoshikage-proxy.before-v2-images-20260912T122346Z`、適用記録は `~/.config/codex-hoshikage-proxy/last-update.json`。

LAN IP経由でready、認証なし401、v1モデル一覧、新しい `response_generated_images / generated_image_artifacts` capabilityを確認。instance・復元世代と既存の保存済み回答が更新前と一致した。専用の検証依頼を実行受付前に取り消し、新しい画像一覧APIで `complete / items: []`、同一キー再送で同じResponse IDとなることを確認した。この配備検証ではAIを起動していない。実画像生成・再起動試験は[受入記録](v2-implementation-status.ja.md)を参照。

Gatewayの管理状態も接続済み、Proxy ready、復元待ち・占有保留なしを確認した。Gateway自体のバイナリ・サービスは変更していない。この配備時点ではDiscordへの画像自動添付の結合確認は未実施だった。後日の確認結果は次節を参照。

## 2026-09-13 Gateway画像表示の確認記録

Gateway側の改定・常駐サービス反映が完了し、利用者からDiscord画像表示のユーザーテスト成功の報告を受けた。Gateway側の[運用・試験記録](../../codex-hoshikage-gateway/docs/implementation-status.ja.md)でもサービス反映と実Discordでの正常動作確認が記録されている。画像表示対応は双方で完了し、Gateway対応待ちは解消した。

この記録更新に伴うProxyの再配備・再起動は行っていない。残る障害復旧・負荷等の確認は[v2受入記録](v2-implementation-status.ja.md)を参照。


## 2026-09-13：対話中継・保存復旧の常駐反映

08:40 JSTに `4446dbb9587674855c315ad3ffada2c63188fc0f` のrelease版を反映した。対話不能時の待機防止、承認失効、v2対話中継API、保存復旧時の同期確認を含む。更新前と停止直後に実行中・UNKNOWN・成果物作成中の依頼がないことを確認。設定・APIキー・環境変数ファイルはハッシュ一致で維持を確認した。

停止中のProxyホームを `~/.config/codex-hoshikage-proxy.before-hardening-20260912T234012Z/`（権限700）へ、旧バイナリを `~/.cargo/bin/codex-hoshikage-proxy.before-hardening-20260912T234012Z` へ退避してから、同じディレクトリ内で実行ファイルを原子的に置換した。管理ソケットは再作成されるためコピー対象外。初回の退避はソケットのコピーで停止し、旧サービスを再開してから取り直した。初回の不完全な退避（`20260912T233930Z`）は復元に使用しない。

適用記録は `~/.config/codex-hoshikage-proxy/last-update.json`。反映したバイナリとrelease成果物のSHA-256は `61586f45742d7c5cdee0e13103254abf79874db48325590165db51a5376fbf1d` で一致。

確認結果：

- LAN経由のready、認証なし401、v1モデル一覧、v2の `interaction_relay` と4対話種別のCapabilityが正常。
- インスタンスID・復元世代、更新前に取得した保存済み回答のバイト列が一致。
- 実行前停止・同一キー再要求・空の画像一覧と新しい対話一覧を検証。
- 常駐Proxyでgpt-5.6-lunaへ短い検証依頼を送り、`DEPLOY_OK`、completed、回答保存readyを確認。同一キーの再要求は同じResponse・Turnを返した。Response IDは `resp_af6aa51e-7a3b-4059-b530-d44633c166f2`。
- systemdはactive/running、確認時の自動再起動回数0、待受は `0.0.0.0:4040`。

Gateway・OpenWebUIのサービスや設定は変更していない。新しい質問／MCP／権限の対話UIは、クライアント側の対応宣言・実装が必要。基本契約2.0と `acceptance_pending` は維持する。

## 2026-09-15 共通MCP・Skill設定の反映

`ff019ee` を22:22 JSTに常駐反映。共通CodexホームのMCP・Skill・プラグインを既定で取り込み、認証・会話DB・実行方針は専用のまま維持する。稼働中の依頼の完了を確認して旧バイナリと永続ホームをバックアップした。コピーはシンボリックリンクを保持し、ライブUnix socketを除外した。

配備後は `active/running`、`NRestarts=0`、既存回答と認証・instance／復元世代の維持を確認。実モデルによるMCP呼び出しと共有Skill読込も成功した。[設定・検証記録](codex-user-extensions.ja.md)を参照。

## 2026-09-15 MCP自動再読込の反映

22:45 JST、`630f0d8` を常駐反映。SHA-256 `ff06b86855b76abed78dd5e29ff276aec5fa5661367848b6ad82e232f23c2ec3`。実行中・結果不明の依頼がないことを確認して、旧バイナリと永続ホームをバックアップした。

反映後は `active/running`、`NRestarts=0`。LAN readiness、認証、v1モデル一覧、v2機能・停止先着・重複抑止、既存保存済み回答、instance／復元世代、APIキー／Proxy設定／環境ファイルの保持を確認。反映前の検証用会話をThread `01a0a53c-862b-7bc3-aa59-3915e42183d6` のまま継続し、実モデル回答・永続保存・同一要求キーの重複抑止も成功した（Response `resp_f7de6c5f-20cc-4422-837d-83522c314449`）。

MCP追加・呼出し・削除の無再起動試験は[隔離環境の実モデル記録](codex-user-extensions.ja.md)を参照。Gatewayへの[承認UI改定依頼](gateway-mcp-approval-change-request.ja.md)は作成済み。Gateway本体の実装・配備と実Discordでの承認結合受入は未完了。

## 2026-09-16 20:31 JST：MCPターン限定許可の接続試験用配備

利用者から「フラグをONにして、常駐環境に反映」「接続テストをする」と明示指示を受け、総合受入前の接続試験用として対応版を配備した。従来の受入後有効化という予定に対し、今回の指示を適用した。総合受入完了を意味しない。

- 未コミットの0.3実装をreleaseビルドして常駐へ反映。基点HEADは77c9991であり、そのコミット単体の配備ではない。
- `[v2] mcp_turn_approval_enabled = true`、`mcp_turn_grant_tools = { playwright = ["browser_find"] }`。他ツールの包括許可は有効にしていない。
- 活動実行・hold・成果物作成がないことを切替前と停止後に確認。停止中の状態・設定一式と旧バイナリを退避。
- 20:31:56 JSTに再起動。active/running、NRestarts=0、`0.0.0.0:4040`、起動エラーなし（既存private extension優先の警告あり）。
- LAN経由readiness、両MCP capabilityのenabled=true／native-item-id-v1、認証なし401、v1モデル一覧、既存回答バイト列一致、instance／復元世代維持を確認。
- AI実行前の停止予約を用いた試験Responseで、受付冪等性・not_started/cancelled・空のinteraction／生成画像一覧を確認。Discordへの試験投稿やAI実行は行っていない。
- バイナリSHA-256：`981a3e257e9f37903031c0ec3a0532dc6cf3f60e11e41416970f2110304da4f7`。
- 配備情報：`~/.config/codex-hoshikage-proxy/last-update.json`。状態退避：`~/.config/codex-hoshikage-proxy.before-mcp-turn-approval-20260916T113154Z`。稼働後の状態を安易に巻き戻さない。

次はGateway／実Discordで操作詳細・単発／ターン許可・取消・次発言での失効を結合確認する。

## 2026-09-17 04:42 JST：MCP公開カード承認0.4の常駐反映

利用者の指示でコミット`bc28861e7bacd51399895c095d6f75aa45119e2b`をreleaseビルドして常駐反映した。Gateway完成の連絡を受領しており、実Discordの結合試験はGateway側で実施予定。

- 活動実行・結果不明・hold・成果物作成がないことを確認後、04:42:29に停止、状態・設定・旧バイナリを退避し、04:42:31に起動。
- 設定変更なし。既存の機能ON、ターン許可対象playwright.browser_findを維持。認証情報・環境ファイル・configのハッシュ一致を確認。
- `mcp_inline_approval.enabled=true`、profile=source-conversation-v1、サイズ／版数上限を確認。既存details/turn機能も有効。
- LAN経由readiness、認証なし401、v1モデル一覧、保存済み回答のバイト列一致、instance／復元世代維持を確認。
- 停止予約済みの試験Responseでapproval_presentationとcontextの受付・同一キーの冪等照会を確認。not_started/cancelled、空のinteraction／生成画像一覧を確認し、AI実行やDiscord投稿は行っていない。
- active/running、NRestarts=0、待受0.0.0.0:4040。新起動のエラーなし（既存private extension優先の警告あり）。実行中バイナリと配置物のSHA-256一致を確認。
- バイナリSHA-256：`90d4a9ac4a317eda955f8adeb0dd82a32bab010cecea3c57bd441f1a178ea818`。
- 状態退避：`~/.config/codex-hoshikage-proxy.before-mcp-inline-04-20260916T194229Z`。旧バイナリ：`~/.cargo/bin/codex-hoshikage-proxy.before-mcp-inline-04-20260916T194229Z`。
- 配備情報：`~/.config/codex-hoshikage-proxy/last-update.json`。試験Response：`resp_19b6446a-01be-4ce7-b106-0836858ebe5c`。

Proxyの接続準備は完了。実Discordの新UI・ボタン操作の受入成功は別途記録する。新たな要求受理後に状態を安易に巻き戻さない。


## 2026-09-17 19:54 JST：汎用MCP承認API 0.6の常駐反映

実装コミット `3a38734595c8312b434144012e49e3d5aebb6697` のreleaseバイナリを反映した。表示・単発承認・明示的な実行ポリシーを分離した合意0.6に対応する。全自動回帰試験、Clippy（警告禁止）、fmt、隔離した実Codex試験を通過後に配備した。

- 最初の更新前確認でlegacy依頼が実行中だったため停止を見合わせた。その後、利用者から当該Codexが暴走しているため再起動してよいとの明示指示を受け、停止・バックアップ・更新を実施した。
- 対象Response `resp_295d8f46-0131-422a-884f-26f35d8c9206` は更新後の上流照会で `interrupted` と確認。元の依頼は再送していない。
- 状態・設定退避：`~/.config/codex-hoshikage-proxy.before-mcp-layered-06-20260917T105418Z`（権限700、シンボリックリンク保持、Unix socket除外）。旧バイナリ：`~/.cargo/bin/codex-hoshikage-proxy.before-mcp-layered-06-20260917T105418Z`。
- バイナリSHA-256：`8367cb4ab07f40fd8488960b9549f8f3338841b7c9c41127bc51109940977bae`。実行中プロセスのバイナリとも一致。適用記録は `~/.config/codex-hoshikage-proxy/last-update.json`。
- DB schema 2→3の移行、instance／復元世代の維持、既存保存回答のバイト一致を確認。APIキー・Proxy設定・環境ファイルはハッシュ一致で維持を確認した。
- LAN経由ready、認証なし401、v1モデル一覧、`mcp_approval_v06.enabled=true`／`source-conversation-v3`、旧MCP機能の互換性を確認。
- 実行前停止済みResponse `resp_4786b9fb-def9-405f-b770-f755e5538b43` で0.6宣言受付、not_started/cancelled、同一キーの重複抑止、空の対話／画像一覧を確認。
- 常駐実Codexの新規検証Response `resp_3c14ed62-736e-4a9f-9da3-4f0b0834ffb7` は `PROXY_V06_DEPLOY_OK` を返してcompleted、回答保存ready。同一キー再送は同じResponseとなった。外部ツール実行・Discord投稿なし。
- systemdはactive/running、NRestarts=0、待受 `0.0.0.0:4040`。既存のターン許可機能ONを維持。0.6では各実行がポリシーを明示選択し、表示だけではポリシーを有効にしない。

利用者の指定に従い、Discordを含む統合試験はGateway側で実施する。Gatewayのコード・サービス・設定は変更していない。Proxy側の実装・単独受入・常駐反映は完了。新規要求を受理済みなので、旧バイナリだけへ戻したり退避DBを安易に上書きしたりしない。

## 2026-09-17 20:52 JST：MCP確認待ち時間の不具合修正

`5b17f9c1cdace5de00cf93a7c5248df1bd24f476` の修正版を常駐反映した。カタログ更新待ちで承認表示が変わる不具合、private_required／unavailableの型不整合を修正。[修正報告・受入結果](mcp-approval-review-delay-fix.ja.md)を参照。

- 実行中・結果不明・占有・成果物作成がないことを更新前と停止後に確認。停止中の状態を `~/.config/codex-hoshikage-proxy.before-mcp-review-delay-20260917T115203Z`、旧バイナリを `~/.cargo/bin/codex-hoshikage-proxy.before-mcp-review-delay-20260917T115203Z` へ退避した。
- 新バイナリSHA-256：`be471a349e119fb5854f93cb9c647b0e90bf801d70fb383122dcc7cd8722841b`。実行中バイナリとも一致。
- APIキー・設定・環境ファイルのハッシュ一致、instance／復元世代・schema 3・既存保存回答の維持、LAN readiness、認証なし401、v1モデル一覧、0.6 capabilityを確認。
- 停止先着の試験Response `resp_1753757e-9e48-426c-a36f-50395749f76d` で0.6受付・重複防止・空の対話／画像一覧を確認。
- 常駐実CodexのResponse `resp_808de775-c013-40bd-82bb-19a4bd375999` は `PROXY_V06_DEPLOY_OK` を返してcompleted／回答ready。同一要求キーの再送は同じResponseとなった。
- systemd active/running、NRestarts=0。Gatewayのコード・サービスには変更なし。Discord再接続受入はGateway側の担当。
