# Codex Hoshikage Proxy Tasks

最終更新: 2026-08-17

## 現在地

Phase 1からPhase 7までの主要な縦切り実装は完了している。Hoshikage、ChatGPT、Ollamaのモデル一覧統合、Responses API、Chat Completions、OpenWebUI Pipeの通常ストリーミングまでは実環境で確認済み。

残る最優先事項は、OpenWebUI v0.11.0におけるApprovalの実機ラウンドトリップ検証である。

## 次に進めるタスク

### A. OpenWebUI Approval 実機PoC（最優先）

- [ ] OpenWebUI v0.11.0へ最新版 `openwebui/codex_hoshikage_pipe.py` を反映
- [ ] `PROXY_BASE_URL` とAPI Keyを設定
- [ ] Approval Requestを発生させる安全なテスト操作を用意
- [ ] Proxyの拡張イベントSSEに `approval_requested` が到達することを確認
- [ ] Pipeの `__event_call__` がOpenWebUI画面に承認ダイアログを表示することを確認
- [ ] Codexの `availableDecisions`（accept / accept_for_session / decline / cancel）が選択肢へ投影されることを確認
- [ ] Accept後、同じTurnが継続して完了することを確認
- [ ] Decline / Cancel後、TurnとApprovalが終端状態になり、Permitが解放されることを確認
- [ ] Pipe切断時にTurn cancelが発行されることを確認

### B. Approval境界の統合テスト

- [ ] Approval APIのWire Decision変換を実Codexで確認
- [ ] Approval capabilityなしクライアントのCleanup後 `approval_required` を確認
- [ ] Approval timeoutの実時間動作を確認
- [ ] Approval二重回答と未提示Decisionの拒否をHTTP経路で確認
- [ ] SSE開始後にApprovalエラーが発生した場合のSSE error eventを確認

### C. リカバリと終了処理

- [ ] Proxy再起動後のCodex App Server子プロセス残存がないことを確認
- [ ] Codex App Server異常終了時のpending request処理を確認
- [ ] Proxy再起動後のResponses `previous_response_id` 継続・`thread_not_found`を確認
- [ ] graceful shutdownの実機確認

### D. 受入テストと文書状態更新

- [ ] Responses / Chat Completionsの非Streaming・Streaming受入テストを実行
- [ ] 3 Providerの切り替え受入テストを実行
- [ ] `/v1/models`のProvider別件数と重複なしを確認
- [ ] OpenWebUI v0.11.0受入結果をRequirements / System Designへ反映
- [ ] 要件・設計書の状態を Draft / Decided / Implemented / Verified へ更新

## 承認方式の現在の設計

1. Codex App ServerがApproval RequestをJSON-RPC server requestとして送る。
2. ProxyがApprovalを状態として保持し、拡張イベントSSEでPipeへ通知する。
3. PipeがOpenWebUIの `__event_call__` でダイアログを表示する。
4. Pipeはユーザーの選択をApproval APIへ返す。
5. ProxyがDomain Decisionへ変換してCodexへ応答する。
6. 同じTurnの継続結果をPipeへストリームする。

未確定なのはこの設計ではなく、OpenWebUI v0.11.0の実環境で `__event_call__` の入力・戻り値形式が現在のPipe実装と一致するかである。

