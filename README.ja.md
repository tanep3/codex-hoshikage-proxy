**日本語** | [English](README.md)

# Codex Hoshikage Proxy

Codex Hoshikage Proxy は、Codex を利用するAIを、ひとつの OpenAI互換エンドポイントから
使えるようにするセルフホスト型プロキシです。Hoshikage、ChatGPT/Codexのサブスクリプション
モデル、OllamaをCodex App Serverへ接続し、モデル一覧とAPIを統合して提供します。
OpenWebUIからも利用できます。

## できること

- `GET /v1/models`、`POST /v1/responses`、`POST /v1/chat/completions` を提供。
- `hoshikage/...`、`chatgpt/...`、`ollama/...` の公開モデルIDでプロバイダとモデルを選択。
- テキスト、ツール呼び出し、使用量、終了状態、エラー、キャンセルをストリーミング。
- Codexのシェル実行・ファイル操作などのエージェント機能を利用。
- 作業ディレクトリの許可リストと、ツール実行時の承認を管理。
- OpenAI APIキーを使わず、ChatGPT/Codexサブスクリプション認証を利用。
- ローカルのHoshikage/OllamaとChatGPTモデルを同じエンドポイントで切り替え。
- 付属PipeによってOpenWebUIへ登録。

## 利用者にとってのメリット

クライアントごとにプロバイダ設定をやり直す必要がありません。プロキシを一度設定すれば、
統合されたモデル一覧から選ぶだけで、ローカルモデルとサブスクリプションモデルを同じ操作で
切り替えられます。Codexがアクセスできるディレクトリや、ツール操作の承認も明示的に制御できます。

## 現在の状態

セルフホスト利用を想定したMVPです。Codex CLI/App Server `0.147.0`以降を対象とし、
OpenWebUI連携はOpenWebUI `v0.11.0`と標準Pipeイベントを対象とします。OpenWebUI標準の承認
ダイアログには現在2ボタンの制約があります。詳細は[OpenWebUI登録ガイド](docs/openwebui.ja.md)を参照してください。

## まず読むガイド

- [インストールガイド](docs/installation.ja.md)
- [ユーザー／APIガイド](docs/user-guide.ja.md)
- [OpenWebUI登録ガイド](docs/openwebui.ja.md)
- [内部要件定義](docs/codex-hoshikage-proxy-requirements.md)／[システム設計](docs/codex-hoshikage-proxy-system-design.md)

## ライセンスと著作者

Copyright (c) 2026 Tane Channel Technology。MIT Licenseです。詳細は[`LICENSE`](LICENSE)を参照してください。

