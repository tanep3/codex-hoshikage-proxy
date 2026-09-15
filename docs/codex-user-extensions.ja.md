# 共通Codex設定のMCP・Skill・プラグイン継承

Proxyは起動時に、同じOSユーザーのCodexホームから拡張設定を取り込みます。Discord GatewayやOpenWebUIはProxyのApp Serverを利用するため、登録の複製をクライアントごとに行う必要はありません。

## 設定と反映

Proxyの `config.toml`:

```toml
[codex]
inherit_global_config = true
# 通常は指定不要。別のCodexホームを参照する場合は絶対パスを指定。
# user_home = "/home/tane/.codex"
```

既定は有効です。参照元は `codex.user_home`、環境変数 `CODEX_HOME`、`$HOME/.codex` の順です。ただし環境変数がProxy専用ホームを指す場合は `$HOME/.codex` を使います。明示した参照元とProxy専用ホームの重複は拒否します。

共通設定の追加・変更・削除後は、実行中の依頼が終わってから `systemctl --user restart codex-hoshikage-proxy` で反映します。新規会話で確認してください。既存の会話ID・保存済み回答は維持されますが、すでにロードされたThreadへのツール変更の即時反映は保証しません。

`inherit_global_config = false` で取り込みを停止できます。Proxyが作成した共有リンクと取り込んだ設定を次回起動で除去し、Proxy専用のSkillは保持します。生成先 `codex-home/config.toml` は起動時に再生成するため手編集しないでください。

## 継承範囲

- `mcp_servers` のコマンド、URL、環境変数、明示された認証設定、ツール制限など。
- `skills`、`plugins`、`marketplaces`、`apps` と対応する拡張機能フラグ。Skillの無効化にはCodex仕様どおり `skills.config[].path` に `SKILL.md` のパスを指定します。
- 共通ホームの `skills/` のユーザーSkill、`plugins/cache/` のプラグイン、`.tmp/bundled-marketplaces/`・`.tmp/marketplaces/` のカタログをシンボリックリンクで参照します。`.system` はProxy側のCodexが管理します。
- 同名のProxy専用ディレクトリやユーザー作成リンクは上書きしません。競合時は起動ログに警告し、専用側を優先します。
- 同じOSユーザーの `~/.agents/skills` と作業ディレクトリ由来のSkill探索はCodex自身の探索規則に従います。

モデル・sandbox・承認方針・プロジェクトの信頼設定・履歴・メモリ設定は取り込みません。これらはProxyの実行方針を維持します。`auth.json`、OAuthの資格情報ストア、セッション、SQLite DBも共有しません。設定ファイル中に明示されたMCP認証値は生成先に含まれるため、生成ファイルは0600、専用ホームは0700で管理します。

## 利用条件と責務

MCPの実行コマンドと参照する環境変数はsystemdの環境でも利用可能である必要があります。OAuthが必要なMCPでは、Proxy専用 `CODEX_HOME` で追加ログインが必要になる場合があります。デスクトップ専用のブラウザー接続・GUI・Cookieなどが同じように使えることまでは保証しません。

設定継承のためのGateway変更は不要です。ただしMCPが利用者へ承認・フォーム入力などを要求する場合、Gatewayは[対話API](interaction-api.ja.md)の対応宣言とUIが必要です。未対応の対話をProxyが勝手に承認することはありません。Proxyは設定・実行・権限を、GatewayはDiscordの表示・認可・回答操作を担当します。

## 検証

2026-09-15、Codex 0.153.4の隔離App Serverで次を確認しました。

- `lightpanda` 32ツール、`node_repl` 4ツール、`serena` 23ツールを取得。
- `lightpanda.session_list` の読み取り専用呼び出しが成功（取得内容はログに出さない）。
- `talmon-browser`、`sites:sites-building`、`sites:sites-hosting`、`visualize:visualize` を認識。
- `sites`・`visualize` プラグインがinstalled/enabled、カタログ読込エラーなし。
- テスト専用Skillの無効化設定を実App Serverで確認。
- 設定削除、壊れた設定、専用データ保持、途中終了後のリンク回復、親ディレクトリ経由の書込逸脱拒否を自動試験。

実環境プローブは `cargo test --locked --test live_user_extensions -- --ignored --nocapture`。この試験は当サーバーの共通設定・Proxy認証を読み、隔離ホームから登録済みMCPへ接続します。モデル実行・Discord投稿は行いません。通常の自動試験からは除外しています。sandbox内ではMCP依存キャッシュにアクセスできずSerena起動を確認できなかったため、常駐相当の権限で再検証しました。

参照：[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)、[Skill](https://learn.chatgpt.com/docs/build-skills)、[設定項目](https://learn.chatgpt.com/docs/config-file/config-reference)。
