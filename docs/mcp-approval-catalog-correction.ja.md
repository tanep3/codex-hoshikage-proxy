# MCP承認全体是正 — カタログ取得の詳細設計

2026-09-17。接続合意版0.5の内部設計。製品実装・受入前。カタログだけの先行リリースは行わない。[API契約案](mcp-approval-full-fix-api.ja.md)と一体で適用する。

## 1. 確認した不具合と設計根拠

旧実装は1ページ1MiB、取得全体10秒、失敗をnullへまとめ全定義を破棄し、最後にunsupported_rendererと表示していた。保存済み実構成は4サーバー342ツール、full応答相当1,076,442 bytes。定義取得失敗と未対応形式を混同するコード経路を確認した。承認迂回の発生を確認したという意味ではない。

[実証記録](mcp-approval-full-fix-validation.ja.md)では、Codex 0.153.4の同じ全構成を隔離して検証した。

- Thread指定・toolsAndAuthOnly・limit=1：4ページ、30.47秒、342件。
- Thread指定・toolsAndAuthOnly・limit=100：1ページ、10.30秒、1,026,691 bytes、342件。
- サンドボックス内ではSerenaがfailedになり23件欠落した。この回は不合格とし、制限外の再試験と分離した。

したがって、limit=1や同期10秒取得を固定する案は採用しない。resource一覧を省いたThread単位の取得、非同期の共有、失敗理由の保持を採用する。1MiB未満だった1回の成功だけで今後の容量を保証しない。

根拠：[OpenAI App Server公式資料](https://learn.chatgpt.com/docs/app-server)のmcpServerStatus/list、導入版生成schemaのListMcpServerStatusParams／McpServerStatusUpdatedNotification、および上記の実測。公式資料はページングとtoolsAndAuthOnlyを確認、Thread指定の実動作は導入版のschemaと隔離実証で確認した。

## 2. 取得単位・処理

keyは `(instance, recovery_generation, runtime_id, thread_id, config_generation)`。runtime_idはプロセスごとに新しい不透明IDを発行する。Thread間で「同じツール名だから同じ定義」と推測しない。静的評価済みschemaの格納自体は共有してよいが、現在のThreadで確認したという証拠は共有しない。

`mcpServerStatus/list` にthreadId、detail=toolsAndAuthOnly、limit=100を送る。nextCursorがnullになるまで続ける。cursorの空・不正型・重複、サーバー名／ツール名の重複、不正なJSON型を検証する。全ページを取得し終えるまで部分的成功を有効catalogとして公開しない。

全サーバーを対象にする。Playwrightだけの抽出を廃止する。取得時にツール定義の正規化ハッシュと評価済みRegistryとの一致、runtimeStatus/authStatus、ツール所属を保存する。descriptionやreadOnlyHintを根拠に未評価のツールを信頼登録しない。

全ページの通信が成功しても、個別サーバーのruntimeStatus=failed/starting/cancelledや再認証要求は別に保持する。失敗したサーバーをtools=[]の正常状態へ読み替えない。そのサーバーのcallはcatalog_server_unavailable/catalog_auth_required。他の正常サーバーの確認済みcallを同じ理由で未対応にしない。runtimeStatus自体がない旧版は検証対象外とし、能力不足を明示する。

## 3. 有限の上限

| 対象 | 上限・動作 |
| --- | --- |
| catalog 1ページのJSON result | 8MiB。超過はcatalog_too_large |
| 1回の全ページ合計 | 32MiB。超過は全体を不採用 |
| ページ数 | 64 |
| cursor／server名／tool名 | cursor8192、server/tool各1024 UTF-8 bytes |
| 全ツール数 | 4096、超過時切捨て禁止 |
| 取得全体 | キュー待ちを含め60秒。timeout時に旧値を使わない |
| RPC1回 | 30秒以下、全体期限の残りを超えない |
| 同時catalog取得 | runtime当たり2、同じkeyは1つのworkerを共有 |
| 取得待機key数 | 32。超過はcatalog_capacity。要求ごとに無制限taskを作らない |
| キャッシュkey数 | 32。使用中の操作を黙ってevictせず、容量不足は明示 |
| キャッシュ総データ量 | 32MiB相当の管理予算。生の全カタログをThread数分複製しない |
| 成功値の有効期間 | 単調時計で取得完了から30秒 |
| 失敗共有期間 | 2秒。連打で同じ失敗RPCを並列発行しない |
| HTTP GET待機 | 250ms。取得完了を待ち続けずcatalog_loadingを返す |

数値は製品仕様として試験する。RSSはJSON表現と同じbytesではない。キャッシュ予算には文字列容量・ノード・mapの管理量を含めて計上し、受入ではプロセス実メモリの増加も計測する。測定前に性能受入済みと言わない。

### 上流readerとメモリ

パース済みValueを作ってからサイズを見るだけでは、大きな上流応答によるメモリ増加を防げない。JSON-RPC envelopeを借用RawValueとして識別し、catalogのresult長を検査してからValue化する。idとresultのJSON上の出現順に依存しない。catalogは深さ64、ノード262144、文字列総量8MiB以内として制限付きvisitorで読み、上限違反をcatalog_invalidへ分ける。

JSONLフレーム自体にも256MiBの読取上限を設け、read_lineで無制限増加させない。この上限はcatalogの許可上限ではない。超過した未完了フレームは同期回復を推測せずtransport failureとし、実行結果を再送しない。既存の画像・回答・大きいツール結果との互換試験を必須にし、256MiBを理由なく既存正常経路へ適用して壊していないことを確認する。これは同じ全体是正に含むreaderの資源管理で、別の暫定修正として配備しない。

## 4. 状態遷移とHTTPへの返却

| 現在 | 事象 | 次の状態・効果 |
| --- | --- | --- |
| absent | 対象Threadが確定した実行開始前、またはGET | queued。worker登録。GETはloading |
| queued | 実行枠取得 | loading |
| loading | 全ページ成功、開始時世代と現在世代一致 | ready。30秒の有効期限を設定 |
| loading | timeout/RPC/形式/容量エラー | failed。理由を保持、旧値無効、epoch更新 |
| loading | 世代変更・runtime終了 | invalidated。遅着した結果は保存しない |
| ready | 同世代GET／承認 | 有効期限内ならsnapshotを使う |
| ready | TTL満了 | stale。自動許可の適用を待機、workerへ再取得要求。grant自体は失効させない |
| stale | 同じ定義で再取得成功 | ready。表示内容が同じなら版を増やさない。元表示／grantのTTLは延長しない |
| ready/stale | 定義変更・対象server障害 | 対象の定義世代更新。旧表示・旧grantを適用しない |
| failed | 2秒後の再照会 | queued。旧成功値へのfallbackは禁止 |

Threadの準備完了時に取得を開始し、承認が来て初めて10秒以上待つことを避ける。catalogだけが未取得でも実行依頼を勝手に再送しない。承認が必要になった場合は現在状態を通知して待つ。

Gatewayはloadingや再試行可能な障害だけを2秒以上の間隔で再照会する。UIは同じカードを更新する。SSE通知を見逃してもGETで復旧できるため、新しい必須SSEイベントは追加しない。

## 5. 取消・競合・世代

- HTTP接続が切れても、共有workerを要求ごとに作り直さない。workerは全体deadline、runtime終了、所有する実行の終了によって有界に終了する。
- RPC登録は取消安全なguardで管理し、timeout／future破棄／書込失敗／正常応答の全経路でpendingから回収する。遅着応答は未知IDとして無視し、別要求へ割り当てない。上流で既に始まった読取の中止完了を保証したとは扱わない。
- 上流通信中はDB transaction・承認ロックを持たない。結果の確定時に「承認処理→DB→メモリ」の順で現在世代と照合する。
- MCP reload成功、設定変更、server startup状態変更、通知欠落、runtime再起動、正式復元は関連keyを無効化する。threadId=nullの通知は該当serverを持つ全keyが対象。
- 取得失敗後に同じ定義へ戻っても、失敗前のepoch／表示／grantを復活させない。単なる成功TTL更新と、失敗からの復帰を区別する。
- 確認済み定義と実サーバー内部の無通知の挙動変更が常に一致するという保証はしない。運用者が接続・実装を信頼し、更新時にreload／世代更新を行う既存の境界を維持する。

### 後続callの更新待機

[API第12節](mcp-approval-full-fix-api.ja.md#12-gatewayレビューr-02定期更新と後続呼出し)を正本とする。catalogのTTL切れだけなら既存grantはactive、一覧のavailabilityはrefreshing。対象callは同じpending interactionで有界待機し、同一世代・同一定義への正常更新後にProxyが既存grantと全実引数を再評価する。Gatewayによる許可送信や再確認カードの連投はしない。grant.stateのsuspended（上流返信結果不明）へ読み替えない。

待機期限はcall受付から60秒、共有worker期限、interaction期限、grant期限の最小。TTL更新で再設定しない。未回答16件の上限を共用する。grant期限切れ／取消／停止／Steerは直ちに待機から除外する。これだけで共有catalog取得を失敗とはしない。正常更新でもそのcallが不適格なら個別確認へ進む。catalog失敗では当該keyの旧grantを失効させ、個別server異常の場合は対象serverへ限定する。復帰で失効済みgrantを復活させない。

拒否・手動許可・取消・停止・更新後の自動適用は既存の送信意思境界で直列化する。1callに1回だけ確定し、確定後の遅着や重複GETで再適用しない。ユーザーが拒否していない更新失敗をuser rejectedとは記録しない。

## 6. 受入

C01: 実342件と、Apps部分を1MiBより大きくした合成catalogでfind/navigate/tabs-listがready。代替の本人向け画面へ落ちたら不合格。
C02: 全上限の直前・ちょうど・直後、cursor循環、重複、schema違反、server failed、再認証。
C03: 同key100並列GETでworker1、別key3以上で実行枠2、待機枠とメモリ予算の限界。
C04: 9／11／29／31／59／61秒相当の遅延、GET250ms、失敗2秒、成功30秒、壁時計後退。
C05: 取得取消後のpending回収、遅着の無害化、世代変更・通知欠落・復元、旧grantの非復活。
C06: 大きいフレーム・深いJSON・ノード数超過を隔離環境で試験。画像・回答・既存v1/v2回帰を含める。
C07: 実構成で所要時間とRSSを記録し、通常操作・本人向け表示・実Discord受入と区別する。

C08: 依頼中許可を作成し30秒経過後に同じ通常callを発生させる。更新中は上流への許可送信0回、grant active/availability refreshing、同一pending interaction。同一定義更新成功後は既存grantで送信意思1件、追加の許可クリック0回、grant TTL不変。
C09: C08中にgrant失効・取消・Stop・Steer・手動拒否・手動許可・catalog失敗・定義変更をそれぞれ競合させる。先に確定した境界へ従い、自動／手動合計送信意思1件以下。障害から復帰しても旧grantで自動適用しない。
C10: 他callと共有したworkerの待機中、1callのgrant期限切れだけで他の正常callやworkerを失敗にしない。個別server障害とcatalog全体取得失敗の失効範囲を検証する。
