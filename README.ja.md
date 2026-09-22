**日本語** | [English](README.md)

# Codex Hoshikage Proxy

## いつものOpenAI互換APIから、Codexを使う

OpenAI APIに対応したアプリを、もう使っていますか？

そのアプリの接続先をこのProxyに変えるだけで、同じような操作感でCodexを使えるようになります。

OpenWebUIや自作スクリプトなどから、ChatGPT/Codexのサブスクリプションモデル、Hoshikage、Ollamaを選んで使えます。

## 何がうれしいの？

- **接続先はひとつだけ。** アプリ側を毎回設定し直さず、モデルを選ぶだけでプロバイダを切り替えられます。
- **Codexのサブスクを使える。** ChatGPTアカウントでCodexにログインすれば、そのアカウントで使えるCodexモデルを利用できます。ChatGPTログインならOpenAI PlatformのAPI Keyは不要です。
- **ローカルとクラウドを使い分けられる。** HoshikageやOllamaをローカルで使い、必要なときだけChatGPTモデルへ切り替えられます。
- **Codexの得意技が使える。** ファイルを読んだり、コマンドを実行したりできます。承認確認とアクセス可能なディレクトリの制限もあります。
- **OpenWebUIで使える。** 付属のPipeを登録すれば、OpenWebUIのモデル選択画面から使えます。

## 何ができるの？

統合されたモデル一覧から選んで、いつものOpenAI形式のリクエストを送れます。

```text
chatgpt/gpt-5.6-luna
hoshikage/unsloth-gemma4-12b-qat-thinking-off
ollama/gemma4:e4b
```

提供する機能は次のとおりです。

- OpenAI互換のモデル一覧 `GET /v1/models`
- Responses API `POST /v1/responses`
- Chat Completions API `POST /v1/chat/completions`
- テキスト差分と完了・失敗イベントのストリーミング、切断時のTurn中断
- 画像入力（URL／data URL）とJSON Schemaによる出力指定
- Codexによるファイル操作・シェル実行、承認処理、作業ディレクトリの許可リスト
- ChatGPTモデルだけに適用できる推論レベル指定
- 永続化した要求ID照会、重複実行防止、TurnのSteer／中断、同一Provider内の会話モデル変更。詳細は[制御API v1](docs/control-api.ja.md)。
- Codex App ServerのJSON-RPCを双方向に利用するCodex Native API `/codex`。

つまり、使い慣れたOpenAI互換クライアントからCodexを呼び出すための橋渡しです。

## はじめ方

1. Codex CLIとこのProxyをインストール。
2. サンプル設定を `~/.config/codex-hoshikage-proxy/config.toml` へコピー。
3. 使いたいプロバイダと、アクセスを許可するディレクトリを設定。
4. ChatGPTモデルを使うならCodexへログイン。
5. ユーザー権限のsystemdサービスとして起動。

詳しくはこちら:

- [APIリファレンス（全エンドポイント）](docs/api-reference.ja.md)
- [インストールガイド](docs/installation.ja.md)
- [ユーザー／APIガイド](docs/user-guide.ja.md)
- [OpenWebUI登録ガイド](docs/openwebui.ja.md)

Codex CLI/App Server `0.147.0`以降を対象とし、今回の実接続検証は`0.153.4`＋`gpt-5.6-luna`で実施しました。
OpenWebUI `v0.11.0`の過去の受入記録はありますが、画像・構造化出力はProxy APIへの直接接続で検証しています。OpenWebUI画像対応とGatewayによるDiscord画像表示の最新結果は[クライアント受入記録](docs/client-acceptance.ja.md)を参照してください。

制御APIの要求照会・重複防止・Steer・中断、および`gpt-5.6-luna`から`gpt-5.6-terra`への同一Thread内変更と再起動後の継承も実接続確認済みです。

## 動作確認と残る制約

通常応答、会話継続、画像入力（`detail=high`）、JSON Schema、ストリーミング、切断時の中断、承認キャンセル／期限切れ、App Server異常終了時のエラー通知を実環境で確認しました。

- `detail=low`で画像の色を誤認する現象が、このモデルではApp Serverへの直接接続でも再現しています。画像には当面`high`を使用してください。
- クライアント定義のツール呼び出し／結果・usageの完全なOpenAI形式変換は未対応です。Codex固有機能はOpenAI互換形式へ推測変換せず、`/codex`の双方向JSON-RPCで利用できるよう改定中です。
- App Server異常終了時はProxyも異常終了します。付属systemdサービスでは5秒後に再起動します。実行中の処理は自動再実行しません。
- v1の最終出力再取得とSSEの過去イベント再生は未対応です。制御APIは共有運用者向けで、利用者ごとの分離はありません。
- 長時間・高負荷運転や、全モデル／全クライアントの互換性は未検証です。

2026-09-13の追加修正と検証・配備状況は[追加受入記録](docs/proxy-hardening-2026-09-13.ja.md)を参照してください。

詳細は[対応表](docs/app-server-coverage.md)、[実接続テスト結果](docs/live-codex-validation.md)、[開発時の検証手順](docs/development.md)を参照してください。

## ライセンス

Copyright (c) 2026 Tane Channel Technology。[MIT License](LICENSE)です。
