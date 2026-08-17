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
- テキスト、ツール呼び出し、usage、完了状態、エラー、キャンセルのストリーミング
- Codexによるファイル操作・シェル実行、承認処理、作業ディレクトリの許可リスト
- ChatGPTモデルだけに適用できる推論レベル指定

つまり、使い慣れたOpenAI互換クライアントからCodexを呼び出すための橋渡しです。

## はじめ方

1. Codex CLIとこのProxyをインストール。
2. サンプル設定を `~/.config/codex-hoshikage-proxy/config.toml` へコピー。
3. 使いたいプロバイダと、アクセスを許可するディレクトリを設定。
4. ChatGPTモデルを使うならCodexへログイン。
5. ユーザー権限のsystemdサービスとして起動。

詳しくはこちら:

- [インストールガイド](docs/installation.ja.md)
- [ユーザー／APIガイド](docs/user-guide.ja.md)
- [OpenWebUI登録ガイド](docs/openwebui.ja.md)

Codex CLI/App Server `0.147.0`以降、OpenWebUI `v0.11.0`を対象としています。

## ライセンス

Copyright (c) 2026 Tane Channel Technology。[[MIT License](LICENSE)]です。

