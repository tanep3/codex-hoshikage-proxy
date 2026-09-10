# Codex App Server対応状況

確認日: 2026-09-11。参照: [公式App Server仕様](https://learn.chatgpt.com/docs/app-server)。
今回追加した入力・出力・ページ送りの項目は、ローカルCodexの`app-server generate-json-schema`出力とも照合済み。
この表はOpenAI互換プロキシとしての主要経路の確認結果であり、App Server全APIの実装を意味しない。

| 機能 | Proxyの対応 |
| --- | --- |
| initialize / initialized | 対応済み |
| 異常終了・自動復旧 | エラー通知は実機確認済み。自動再起動は未実装、Proxy再起動が必要 |
| model/list | 今回ページ送りを追加。対応推論強度を取り込み |
| thread/start、既存Threadの継続 | 対応済み。Responsesのprevious_response_idをThreadへ対応付け |
| turn/startのテキスト入力 | 対応済み。今回Responsesのメッセージ形式にも対応 |
| turn/startの画像入力 | 今回HTTP(S)・画像data URL・detail指定を追加 |
| turn/start.outputSchema | 今回Responsesのtext.format、Chatのresponse_formatから転送を追加 |
| turn/start.effort | Responsesに加えて今回Chatのreasoning_effortにも対応 |
| turn/interrupt | 切断・タイムアウト時の内部処理で使用。独立した操作APIは未公開 |
| コマンド実行・ファイル変更の承認 | 対応済み。文字列ID・通知遅延・ワークスペース内判定を修正済み |
| ローカル画像、skill入力、toolOutput | 未対応。ローカルパス境界やツール結果の対応付けが必要 |
| turn/steer、review/start、手動compact、Thread管理API | 未公開 |
| tool/requestUserInput、MCP elicitation | 対話応答の中継は未対応。現状これらを要求するTurnは待機する可能性がある |
| permissions/requestApproval | 権限専用の応答形式は未対応。コマンド／ファイル承認と同じものとして利用できない |
| usage・ツール実行イベントの完全なOpenAI互換変換 | 未完成。通常の最終テキスト経路とは別途対応が必要 |

今回の自動テストは模擬App ServerとのHTTP往復と送信パラメータを検証する。
実Codex接続も実施済み。data URL画像・Schema・ストリーミング・切断等の結果と、detail=lowで再現した画像誤認は[実接続テスト結果](live-codex-validation.md)を参照。HTTP(S)画像取得は今回未検証。
