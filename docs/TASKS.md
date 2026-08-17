# Codex Hoshikage Proxy Tasks

最終更新: 2026-08-17

## 現在地

Phase 1からPhase 7までの主要な縦切り実装は完了している。Hoshikage、ChatGPT、Ollamaのモデル一覧統合、Responses API、Chat Completions、OpenWebUI Pipeの通常ストリーミングまでは実環境で確認済み。

OpenWebUI v0.11.0におけるApprovalのAcceptラウンドトリップとtimeoutの実時間動作は実機確認済み。
timeout時はProxy側のCleanupとCodexへの拒否を確認済みだが、標準Confirmation Dialogが画面に残る。
これはOpenWebUI本体を改修しない前提で既知の運用制約とする。キャンセル系・切断系は実機確認済みで、残るのは受入テスト全体の実機確認である。

## 次に進めるタスク

### A. OpenWebUI Approval 実機PoC（最優先）

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

- [ ] Approval APIのWire Decision変換を実Codexで確認
- [x] Approval capabilityなしクライアントのCleanup後 `approval_required` を確認（Fake Codex HTTP統合テスト）
- [x] Approval timeoutの実時間動作を確認（Proxy Cleanup済み、標準Dialogは残ることがある）
- [x] Approval二重回答と未提示Decisionの拒否をHTTP経路で確認（Fake Codex HTTP統合テスト＋Domainテスト）
- [x] SSE開始後にApprovalエラーが発生した場合のSSE error eventを確認（Fake Codex HTTP統合テスト）

自動テストでは、Domain／Approval Managerの二重回答拒否、未提示Decision拒否、
`approval_required`のThread分離、ApprovalイベントのTurn分離を確認済み。残る項目は
実Codexを使ったApproval APIの受入確認と、二重回答の実HTTP経路確認である。Fake Codexを
使ったHTTP統合テストでは、非対話クライアントの`approval_required` SSEとストリーム終了を確認済み。

### C. リカバリと終了処理

- [x] Proxy再起動後のCodex App Server子プロセス残存がないことを確認
- [ ] Codex App Server異常終了時のpending request処理を確認（任意の追加確認。Fake Codex自動テスト済みでリリース阻害要因ではない）
- [x] Proxy再起動後のResponses `previous_response_id` 継続・`thread_not_found`を確認
- [x] graceful shutdownの実機確認

Fake Codex統合テストでは、Codex transport終了時のpending request解決と、shutdown時の
子プロセス終了待ち・RuntimeのStopped遷移を確認済み。実Codexを用いた確認は未実施である。

### D. 受入テストと文書状態更新

- [ ] Responses / Chat Completionsの非Streaming・Streaming受入テストを実行
- [x] 3 Providerの切り替え受入テストを実行
- [ ] `/v1/models`のProvider別件数と重複なしを確認
- [x] Hoshikage詳細カタログでTool Calling非対応モデルがProxy一覧から除外されることを実機確認
- [x] OpenWebUI v0.11.0受入結果をRequirements / System Designへ反映
- [ ] 要件・設計書の状態を Draft / Decided / Implemented / Verified へ更新

## 承認方式の現在の設計

1. Codex App ServerがApproval RequestをJSON-RPC server requestとして送る。
2. ProxyがApprovalを状態として保持し、拡張イベントSSEでPipeへ通知する。
3. PipeがOpenWebUIの `__event_call__` でダイアログを表示する。
4. Pipeはユーザーの選択をApproval APIへ返す。
5. ProxyがDomain Decisionへ変換してCodexへ応答する。
6. 同じTurnの継続結果をPipeへストリームする。

OpenWebUI v0.11.0の標準UIは4ボタンを提供しないため、Pipeは二択のConfirmation Dialogを使用する。Accept経路は実環境で確認済み。timeout後はProxy側でApprovalとTurnが終端になるが、OpenWebUI標準UIにはPipe／Proxyからダイアログを閉じるイベントがないため、画面上にダイアログが残ることがある。この挙動は既知の運用制約とする。
