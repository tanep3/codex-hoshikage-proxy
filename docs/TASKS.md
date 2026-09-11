# Codex Hoshikage Proxy Tasks

最終更新: 2026-09-11

## 現在地

通常利用の主要APIと停止・切断経路の信頼性を改善した。全機能の受入完了や長時間稼働の保証とは区別する。
2026-09-11にCodex 0.153.4＋gpt-5.6-lunaで以下を実接続確認した。

- [x] 初期化、モデル一覧、通常応答、同一稼働中のprevious_response_id継続
- [x] Responses／Chatのストリーミング
- [x] data URL画像（detail=high）、JSON Schema、Chatの推論強度
- [x] クライアント切断時のTurn中断
- [x] 提示されたcancelでの承認拒否、二重回答への409、承認期限切れ
- [x] 隔離App Serverの強制終了からHTTPストリームへの即時エラー通知
- [x] 低詳細画像の誤認を直接App Server接続でも再現し、Proxy固有の問題ではないことを確認

画像detail=lowの不一致は未解消。全自動テストは56件で、Clippyも警告なし。
試行履歴・テスト側の誤判定修正・未検証境界は[実接続テスト結果](live-codex-validation.md)に記録する。

次の優先事項は、長時間・並列負荷試験、未対応の対話要求とイベント変換である。
[対応表](app-server-coverage.md)を現在の実装範囲の基準とする。

下記OpenWebUI／他Providerの実機記録は従来の受入履歴を保持したもので、今回のAPI直接接続テストで再検証したものではない。
OpenWebUI v0.11.0ではtimeout後も標準Confirmation Dialogが画面に残る制約が記録されている。

## 次に進めるタスク

### A. OpenWebUI Approval 実機PoC（過去の受入記録）

- [x] OpenWebUI v0.11.0へ最新版 `openwebui/codex_hoshikage_pipe.py` を反映
- [x] `PROXY_BASE_URL` とAPI Keyを設定
- [x] Approval Requestを発生させる安全なテスト操作を用意
- [x] Proxyの拡張イベントSSEに `approval_requested` が到達することを確認
- [x] Pipeの `__event_call__` がOpenWebUI画面に承認ダイアログを表示することを確認
- [x] Codexの `availableDecisions` 省略時を含め、二択結果を正しいWire Decisionへ変換することを確認
- [x] Accept後、同じTurnが継続して完了することを確認
- [x] Decline / Cancel後、TurnとApprovalが終端状態になり、Permitが解放されることを確認
- [x] Pipe切断時にTurn cancelが発行されることを確認（ブラウザのリロードで確認）

### B. Approval境界の統合テスト

- [x] Codex 0.153.4が提示したcancelの往復と二重回答拒否を確認
- [ ] 同バージョンでaccept／declineが提示されるケースの往復を確認
- [x] Approval capabilityなしクライアントのCleanup後 `approval_required` を確認（Fake Codex HTTP統合テスト）
- [x] Approval timeoutの実時間動作を確認（Proxy Cleanup済み、標準Dialogは残ることがある）
- [x] Approval二重回答と未提示Decisionの拒否をHTTP経路で確認（Fake Codex HTTP統合テスト＋Domainテスト）
- [x] SSE開始後にApprovalエラーが発生した場合のSSE error eventを確認（Fake Codex HTTP統合テスト）

自動テストでは、Domain／Approval Managerの二重回答拒否、未提示Decision拒否、
`approval_required`のThread分離、ApprovalイベントのTurn分離を確認済み。実Codexでもcancelと二重回答拒否を確認済み。accept／declineなど残りの選択肢は追加確認する。Fake Codexを
使ったHTTP統合テストでは、非対話クライアントの`approval_required` SSEとストリーム終了を確認済み。

### C. リカバリと終了処理

- [x] Proxy再起動後のCodex App Server子プロセス残存がないことを確認
- [ ] Codex App Server異常終了時のpending request処理を確認（任意の追加確認。Fake Codex自動テスト済みでリリース阻害要因ではない）
- [x] Proxy再起動後のResponses `previous_response_id` 継続・`thread_not_found`を確認
- [x] graceful shutdownの実機確認

Fake Codex統合テストでは、Codex transport終了時のpending request解決と、shutdown時の
子プロセス終了待ち・RuntimeのStopped遷移を確認済み。今回、実Codex異常終了時のストリームへの即時エラー通知も確認した。
App Server異常終了時はProxyが非ゼロで終了し、付属systemdサービスが再起動する。SIGTERMによる通常停止は正常終了する。
再起動後のResponses継続ではthread/resumeを使う。実行中だったTurnは自動再実行しない。

### D. 受入テストと文書状態更新

- [x] Responses / Chat Completionsの非Streaming・StreamingをCodex 0.153.4＋gpt-5.6-lunaで確認
- [x] 3 Providerの切り替え受入テストを実行
- [ ] `/v1/models`のProvider別件数と重複なしを確認
- [x] Hoshikage詳細カタログでTool Calling非対応モデルがProxy一覧から除外されることを実機確認
- [x] OpenWebUI v0.11.0受入結果をRequirements / System Designへ反映
- [x] 要件・設計のDraftと現在の実装／検証範囲を分離し、対応表・実接続結果へリンク
- [ ] 長時間・並列負荷試験と他Provider／OpenWebUIでの今回追加機能の受入確認

## 承認方式の現在の設計

1. Codex App ServerがApproval RequestをJSON-RPC server requestとして送る。
2. ProxyがApprovalを状態として保持し、拡張イベントSSEでPipeへ通知する。
3. PipeがOpenWebUIの `__event_call__` でダイアログを表示する。
4. Pipeはユーザーの選択をApproval APIへ返す。
5. ProxyがDomain Decisionへ変換してCodexへ応答する。
6. 同じTurnの継続結果をPipeへストリームする。

OpenWebUI v0.11.0の標準UIは4ボタンを提供しないため、Pipeは二択のConfirmation Dialogを使用する。Accept経路は実環境で確認済み。timeout後はProxy側でApprovalとTurnが終端になるが、OpenWebUI標準UIにはPipe／Proxyからダイアログを閉じるイベントがないため、画面上にダイアログが残ることがある。この挙動は既知の運用制約とする。

## 2026-09-11 汎用制御API v1

- 要求ID照会・同期永続化・開始識別子・重複実行防止を追加。
- 明示中断、期待Turn照合付きSteer、同一Thread競合拒否を追加。
- 承認IDをUUID化し、要求単位の自動承認抑制と継続時維持、有効承認一覧を追加。
- 監視SSEのsnapshot・欠落通知、Capability APIを追加。最終出力の再取得は非対応と明示。
- 改定されたP-09に従い、同一Provider内のモデル変更・最新選択の継承・再起動復元を追加。
- 詳細なAPI・制約・責務境界は[契約回答](control-api.ja.md)。常駐サービスへの適用は別作業。
