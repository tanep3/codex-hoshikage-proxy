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

MCP設定の追加・変更・削除は、Proxyが新規／継続依頼の開始前（thread/start、thread/resume、turn/start）に検出し、`config/mcpServer/reload` を呼んで反映します。チャット再作成・サービス再起動は不要です。既存の会話ID・ワーク・履歴を維持し、実行中のTurnを中断しません。待機中にバックグラウンドで接続を開始する方式ではなく、次の依頼時に反映します。

再読込完了はApp Serverへの設定更新要求の成功であり、各MCPの起動・認証成功までは保証しません。壊れた設定や再読込失敗時は、新しい上流実行を進めずエラーを返します。設定を修正して次の依頼を送れば再確認します。失敗・通信結果不明のAI依頼を自動で再実行しません。

この自動再読込の対象は `mcp_servers` とMCP OAuth設定・起動猶予設定です。Skill・プラグインの登録変更、`inherit_global_config`／`user_home` などProxy設定自体の変更は、引き続き実行完了後にProxyを再起動して取り込みます。これらもチャットの作り直しは要求しません。

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

## 常駐反映結果

2026-09-15 22:22 JST、実装コミット `ff019ee` を常駐反映済み。配備バイナリSHA-256は `6dddaf5083580fa2f755f235e45c0620d95756b16b953445bea793d9a313e62f`。

- サービス `active/running`、`NRestarts=0`。LAN readiness、認証必須、v1モデル一覧、v2機能、停止先着・重複抑止を確認。
- APIキー・Proxy設定・環境ファイルのハッシュ、instance／復元世代、既存保存済み回答を維持。専用Skill `l13-packet-jam-recovery` も保持。
- 実モデル `chatgpt/gpt-5.6-luna` が `lightpanda.session_list` を1回実行し、共有 `talmon-browser/SKILL.md` を読み込んで `EXTENSIONS_OK` を回答。最終文面だけでなくTurn内の完了した `mcpToolCall` と終了コード0の `commandExecution` を照合。Skillのブラウザー操作・投稿ワークフローは実行していない。
- 検証Response `resp_0b83a15f-5e99-4e08-bd24-f67f71c36bd1`、Turn `01a0a53c-8723-7893-a39b-3bd841d75ead`。保存済み回答の再取得と同一要求キーでの重複抑止も成功。この呼び出しでは対話承認要求は発生していない。
- 通常Rustテスト全件、設定テスト9件、追加継承テスト3件、Clippy（warnings禁止）、fmtを通過。実MCP／Skill試験は別途明示実行。

Gatewayサービス・Discord投稿は変更していない。Discord画面からの利用者操作や、各MCPの全機能の受入とは区別する。

Gateway側のMCP承認・入力UIと終了理由表示は、[修正依頼](gateway-mcp-approval-change-request.ja.md)にまとめています。設定再読込だけではDiscordの承認未対応による拒否は解消しません。

## 既存会話へのMCP再読込の受入

2026-09-15、`tests/live_mcp_reload.rs` により、Codex 0.153.4／gpt-5.6-lunaで同じThread `01a0a54d-85ea-7b52-8824-93fa8617dbf9` の3 Turnを検証した。最初はMCPなし、設定追加後は次のTurnでローカルの読み取り専用 `reload_test.reload_probe` が実際にcompleted、削除後のTurnを完了してMCP一覧から消えたことを確認。App Serverを再起動せず、グローバルの本番設定も変更していない。

模擬RPC試験では再読込失敗時にthread/resumeを送らないこと、次回の再読込、変更なし時の再読込抑止、壊れた設定からの秘密値非表示、sandboxの維持、MCP削除を確認。再読込の応答未確認後に元設定へ戻った場合も、生成ファイルを元に戻してから再読込する。

実試験の実行方法：`cargo test --locked --test live_mcp_reload -- --ignored --nocapture`。実モデル呼び出しを伴うため通常の自動試験からは除外する。Gatewayの承認UI試験とは別である。

自動再読込の実装 `630f0d8` は2026-09-15 22:45 JSTに常駐反映済み。反映前の検証用会話の継続も実モデルで成功した。[配備記録](server-operations.md)を参照。
