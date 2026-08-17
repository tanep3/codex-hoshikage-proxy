# インストールガイド

**日本語** | [English](installation.md)

## 1. 前提

- Codex CLIを実行できる環境。
- RustとCargo。
- Codex CLI/App Server `0.147.0`以降。
- Hoshikageモデルを使う場合はHoshikage、Ollamaモデルを使う場合はOllama。
- 付属連携を使う場合はOpenWebUI `v0.11.0`。

確認:

```sh
codex --version
```

Proxyは子プロセスとして `codex app-server --listen stdio://` を起動します。Codexの認証を置き換えたり、
認証情報を作成したりはしません。

## 2. インストールと設定

リポジトリのルートで実行します。

```sh
cargo install --path .
mkdir -p "$HOME/.config/codex-hoshikage-proxy"
cp config.example.toml "$HOME/.config/codex-hoshikage-proxy/config.toml"
```

これで `~/.cargo/bin/codex-hoshikage-proxy` にProxyがインストールされます。Proxyは常駐サービスなので、通常は `cargo run` で起動せず、
ユーザー権限のsystemdサービスとして動かします。

`~/.config/codex-hoshikage-proxy/config.toml` を編集します。

- `server.host` と `server.port` を設定。
- `server.default_cwd` を、実在するディレクトリに設定。
- `security.allowed_cwds` を、実際に許可するディレクトリへ変更。`/home/tane/work`などは例であり必須ではありません。
- 許可ルートとdefault cwdは事前に存在している必要があり、Proxyは自動作成しません。
- 利用するプロバイダを有効化。
- `defaults.model` を設定済みの公開モデルIDへ設定。

LANなど非loopbackで公開する場合はAPI Keyを設定します。

```toml
[security]
api_key = "replace-with-a-long-random-secret"
allowed_cwds = ["${HOME}/work"]
```

または `api_key_env = "PROXY_API_KEY"` で環境変数を使えます。非loopbackでAPI Keyがない場合、起動は拒否されます。
TLSはリバースプロキシへ委譲し、CORSはデフォルト無効です。

## 3. ChatGPT/Codexサブスクリプション認証

ChatGPTプロバイダはCodex App ServerのOpenAI認証を利用します。これはProxy利用者がProxyへ送るAPI Keyとは別物です。

ブラウザが使える端末では、Proxy専用のCodex homeを指定して認証します。

```sh
export CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home"
codex login
```

ヘッドレス環境やリモート環境ではデバイスコード認証を使います。

```sh
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login --device-auth
```

表示されたURLをブラウザで開き、必要なサブスクリプション／ワークスペース権限を持つChatGPTアカウントでログインし、
一回限りのコードを入力します。利用できない場合は、ブラウザが使える端末で認証して認証キャッシュを安全に移します。

Proxy専用の `CODEX_HOME` は管理領域です。Proxyは `config.toml` を生成できますが、`auth.json` は生成しません。
認証はCodex標準手順で行います。`auth.json` はパスワードと同様に扱い、コミット・共有しないでください。

ChatGPT認証はサブスクリプション／ワークスペース権限を使います。API Key認証は別方式で、OpenAI Platformの従量課金です。
ChatGPT Plusの利用枠とは異なります。詳細は[OpenAI公式のCodex認証ガイド](https://learn.chatgpt.com/docs/auth)を参照してください。

ChatGPTモデルを有効化する例:

```toml
[providers.chatgpt]
codex_id = "openai"
enabled = true
max_concurrent_turns = 4

[models."chatgpt/gpt-5.6-luna"]
provider = "chatgpt"
upstream_model = "gpt-5.6-luna"
display_name = "GPT-5.6 Luna"
reasoning_efforts = ["low", "medium", "high"]
default_reasoning_effort = "medium"
```

推論レベルはChatGPTモデルのみ指定できます。他のプロバイダではCodex側のデフォルトを使い、必要な場合は `medium` を推奨します。

## 4. HoshikageとOllama

ローカルHoshikageの例:

```toml
[providers.hoshikage]
codex_id = "hoshikage"
enabled = true
max_concurrent_turns = 1
base_url = "http://127.0.0.1:3030/v1"
```

別ホストのHoshikageには `auth_env_key` を設定し、起動前にトークンを環境変数へ設定します。Hoshikageのモデル一覧を取得し、
ツール呼び出し非対応モデルはCodexのツール利用向け動的一覧へ登録しません。

Ollamaはプロバイダを有効にし、公開モデルIDを設定します。標準のローカルエンドポイントを利用します。

## 5. 認証してサービスを起動

ChatGPTモデルを使う場合は、サービスを起動する前にProxy専用Codex homeでログインします。サービスも同じhomeを自動的に使います。

```sh
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login --device-auth
```

サービスユニットを登録します。

```sh
mkdir -p "$HOME/.config/systemd/user"
cp contrib/systemd/codex-hoshikage-proxy.service \
  "$HOME/.config/systemd/user/codex-hoshikage-proxy.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-proxy.service
```

状態とログの確認:

```sh
systemctl --user status codex-hoshikage-proxy.service
journalctl --user -u codex-hoshikage-proxy.service -f
```

付属ユニットは `~/.cargo/bin/codex-hoshikage-proxy` を使います。`codex` が標準以外の場所にある場合は、ユニットの `Environment=PATH=...` を編集してください。
ログアウト後も常駐させるには、次を一度実行します。

```sh
loginctl enable-linger "$USER"
```

停止・無効化:

```sh
systemctl --user disable --now codex-hoshikage-proxy.service
```

## 6. APIを確認

```sh
curl -H "Authorization: Bearer replace-with-a-long-random-secret" \
  http://127.0.0.1:4040/v1/models
```

loopback専用でAPI Key未設定ならAuthorizationヘッダーは不要です。レスポンスには `provider/model` 形式のモデルIDが含まれます。

## 7. 次に読むもの

- [ユーザー／APIガイド](user-guide.ja.md)
- [OpenWebUI登録ガイド](openwebui.ja.md)
