# Codex Hoshikage Proxy Tasks

最終更新: 2026-08-17

## 現在地

Phase 1からPhase 7までの主要な縦切り実装は完了している。Hoshikage、ChatGPT、Ollamaのモデル一覧統合、Responses API、Chat Completions、OpenWebUI Pipeの通常ストリーミングまでは実環境で確認済み。

OpenWebUI v0.11.0におけるApprovalのAcceptラウンドトリップとtimeoutの実時間動作は実機確認済み。
timeout時はProxy側のCleanupとCodexへの拒否を確認済みだが、標準Confirmation Dialogが画面に残る。
これはOpenWebUI本体を改修しない前提で既知の運用制約とする。残るのは拒否系・切断系と、受入テスト全体の実機確認である。

## 次に進めるタスク

### A. OpenWebUI Approval 実機PoC（最優先）

- [x] OpenWebUI v0.11.0へ最新版 `openwebui/codex_hoshikage_pipe.py` を反映
- [x] `PROXY_BASE_URL` とAPI Keyを設定
- [x] Approval Requestを発生させる安全なテスト操作を用意
- [x] Proxyの拡張イベントSSEに `approval_requested` が到達することを確認
- [x] Pipeの `__event_call__` がOpenWebUI画面に承認ダイアログを表示することを確認
- [x] Codexの `availableDecisions` 省略時を含め、二択結果を正しいWire Decisionへ変換することを確認
- [x] Accept後、同じTurnが継続して完了することを確認
- [ ] Decline / Cancel後、TurnとApprovalが終端状態になり、Permitが解放されることを確認
- [ ] Pipe切断時にTurn cancelが発行されることを確認

### B. Approval境界の統合テスト

- [ ] Approval APIのWire Decision変換を実Codexで確認
- [ ] Approval capabilityなしクライアントのCleanup後 `approval_required` を確認
- [x] Approval timeoutの実時間動作を確認（Proxy Cleanup済み、標準Dialogは残ることがある）
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

OpenWebUI v0.11.0の標準UIは4ボタンを提供しないため、Pipeは二択のConfirmation Dialogを使用する。Accept経路は実環境で確認済み。timeout後はProxy側でApprovalとTurnが終端になるが、OpenWebUI標準UIにはPipe／Proxyからダイアログを閉じるイベントがないため、画面上にダイアログが残ることがある。この挙動は既知の運用制約とする。
