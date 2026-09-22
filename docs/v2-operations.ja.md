# v2の設定・運用

> **廃止済みの履歴資料:** 旧`/v2/codex`向けの運用記録であり、新規運用には適用しない。現行APIは[Codex Native API契約](codex-native-api.ja.md)を参照する。旧DBは自動削除せず、撤去時に退避方針を明示する。

実装対象は[契約0.2](workspace-artifact-api-v2.ja.md)。[受入記録](v2-implementation-status.ja.md)の未確認項目を本番保証に含めない。このサーバーへの配備状態は[常駐運用記録](server-operations.md)を参照。

## 有効化と保存先

Codex CLI 0.153.4を使用する。v2の動的ツールadapterはこの検証済み版に固定し、異なる版では起動を拒否する。更新時はschemaと実接続の再受入を行ってadapterの対応版を更新する。

v2は標準で有効で、バージョン切替設定は不要。`config.example.toml`を使い、`PROXY_API_KEY`環境変数（または`security.api_key`）に十分に長いAPIキーを設定する。`/v1`のOpenAI互換APIも併用できる。明示的な`server.v2_enabled = false`はv2未移行環境の旧動作検証用で、通常運用には使わない。LAN利用時の`server.host = "0.0.0.0"`は従来どおり利用可能。v2追加によるlistenアドレス変更はない。

- 管理ワーク: 保存された`default_cwd`配下の`.managed-workspaces/ws_*`。
- メタデータ・保存物: `$CODEX_HOSHIKAGE_PROXY_HOME/state/v2/`。SQLite WAL、同期commit、同時所有者ロック、ディレクトリ700・保存ファイル600。
- Codex履歴: 同じProxy homeの`codex-home/`。
- v1の既存台帳: 初回v2起動時に`state/responses/`を検証し、`pre-v2/`へ元バイト列とハッシュを退避してSQLiteへ取り込む。以後の要求キー・会話対応は同じDBが正本で、JSONLへの追記は止まる。移行は一度だけ。破損した台帳を無視して再作成しない。

モデルの作業領域とProxy状態領域を分離する。v2は共有運用者のBearer認証であり、Discord利用者ごとの認可はGatewayの責務。保存時暗号化は実装していない。バックアップには会話内容・生成物が含まれるため、保存先にも同等のアクセス制御を適用する。

`GET /v2/codex/capabilities`でinstance・generation・実効制限を取得し、それ以外のv2要求へ`X-Proxy-Instance-Id`と`X-Proxy-Recovery-Generation`を付ける。POSTには`Idempotency-Key`が必要。202を受けたら操作・対象IDを保存して照会する。通信結果が不明な要求を新しいキーでやり直さない。

新しい入力はSQLiteに一時保存され、開始確定／確定拒否で削除される。UNKNOWNの入力は保持期限後に削除するが、要求キーと結果記録は残す。削除は物理媒体からの完全消去を意味しない。

容量設定は`[v2]`に置く。`GET /v2/codex/capacity`で使用・予約量を確認する。期限切れ内容はGCし、操作・成果物の識別情報は残す。ワーク原本は自動削除しない。

## ローカル管理

サーバ実行ユーザー専用の`state/v2/admin.sock`を使う。HTTP Bearer利用者に管理権限は与えない。CLIにはサーバと同じ`CODEX_HOSHIKAGE_PROXY_HOME`と設定ファイルを指定する。

```bash
codex-hoshikage-proxy admin workspace register --operation-id ws-register-001 --path /absolute/shared-work --display-name shared-work
codex-hoshikage-proxy admin workspace revoke --workspace-id ws_example
codex-hoshikage-proxy admin execution-hold inspect --response-id resp_example
```

inspectで返された対象・状態・revision・review_tokenと、上流がまだ書き込んでいる可能性を確認してから解除する。

```bash
codex-hoshikage-proxy admin execution-hold release --operation-id hold-release-001 --response-id resp_example --review-token review_example --expected-revision 2 --reason "上流と残存プロセスを調査した結果と判断理由" --accept-risk
```

解除は実行停止・成功への書換えではない。元要求の再送を禁止し、旧会話を隔離する。再利用は共有ワークを明示選択した新しい会話で行う。後から実行中と判明すると占有が復活する。

## 正式バックアップ・復元

バックアップ前にGatewayをpauseし、活動実行・UNKNOWN・コピーが残っていないことを照会する。

```bash
codex-hoshikage-proxy admin backup create --destination /absolute/new-backup-directory
```

v2 DB、保存本体とmanifest、v1台帳、Codexのsessions・archived_sessions・state SQLite・session indexを含む。SQLiteは整合したコピーを作る。認証情報・設定・ワーク原本はbundleに含めない。manifestの`codex_history`が`included`であることを確認する。原本と設定は別途保全する。原本への外部書込みの整合は保証しない。

復元時はGatewayの受付を止め、Proxyサービスを停止し、旧App Serverと生成した子プロセスが終了していることを確認する。そのうえで次を実行する。

```bash
codex-hoshikage-proxy admin backup restore --from /absolute/backup-directory
```

同じホスト・Proxy home・ワーク配置へ復元する。外部restore-pendingマーカー、新世代、旧状態の退避を作る。途中終了したら同じbundleで同じコマンドを再実行する。マーカーを手で消さない。補助データまで復元が終わるまではサーバ起動を拒否する。

完了後にサービスを起動すると`recovery_blocked`になる。Capabilityと対象照会を使い、Gatewayの未完了記録と照合する。`/readyz`は解除まで503となる。新世代のヘッダーを付けた明示停止は利用できる。

```bash
codex-hoshikage-proxy admin recovery release --restore-id restore_example --generation gen_example --reason "GatewayとProxyの照合結果および残るUNKNOWNの扱い" --accept-risk
```

解除の再送は同じ監査結果を返す。個別UNKNOWNの占有は解除しない。v2状態が存在するhomeで`v2_enabled=false`にして起動することは拒否する。古いバイナリへの切戻しはv2ストアを認識しないため禁止する。復元・切戻し前にバイナリ版、設定、Gatewayの世代を一組で確認する。

## 検証

```bash
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --bin codex-hoshikage-proxy
python3 scripts/live_v2_smoke.py
# 生成画像の実接続受入（別の実モデル試験）
python3 scripts/live_v2_images.py
```

上記のPython試験は実モデルを使用する。現在の認証を一時領域へコピーし、隔離したProxyを起動する。常駐サービスを操作しない。実行にはモデル利用量が発生する。生のCodexログ・認証内容は標準出力に表示しない。

## 生成画像の登録状況

capabilitiesの `response_generated_images` を確認し、`GET /v2/codex/responses/{id}/generated-images` で登録状況を照会する。設定上限、期限、状態とエラーは[生成画像API契約](generated-image-api.ja.md)を参照。画像登録失敗時にAIを再実行しない。画像一覧の期限と成果物本体のリース期限は独立する。対応前に受け付けたResponseの409は、画像なしやAI失敗の意味ではない。


## 保存同期エラーからの復旧

保存本体・manifest・保存先ディレクトリの同期に失敗した場合、保存成功とは扱わない。ハッシュが一致していても、再起動時に再度同期が成功するまで公開しない。ストレージの障害を解消した後、保存済みmanifestと本体が残っていれば、通常のサービス再起動で同じID・バイト列から復旧する。原本を再コピーしたり、AIの依頼を再実行したりして復旧を代用しない。同期が通らない間の再起動だけで障害が解消するとは保証しない。

本体・manifestが不完全な場合は取得不可のまま維持する。実行状態のUNKNOWNと成果物保存のunknownは別の状態であり、この保存復旧は実行占有の管理解除や実行の再送を意味しない。[障害試験記録](proxy-hardening-2026-09-13.ja.md)を参照。

## MCP操作詳細・依頼中の許可

新規クライアントは[API 0.6](mcp-approval-api-v06.ja.md)と[利用者・管理者ガイド](mcp-approval-v06-guide.ja.md)に従います。表示・単発承認と、明示選択する承認ポリシーを分離しました。`mcp_approval_v06`を照会し、既定のポリシー無効でも汎用表示を利用できます。

`[v2] mcp_turn_approval_enabled = true`は、運用者が依頼中許可のポリシーを提供する場合に使用します。クライアントのポリシー指定と利用者の明示許可なしに自動承認しません。従来の`mcp_turn_grant_tools`は旧profileの互換用です。0.6の適格性は評価済みの定義・実引数・選択ポリシーで判断します。

既存の0.3/0.4 Responseは元の形式のまま扱います。旧クライアントの説明は[旧ガイド](mcp-turn-approval-guide.ja.md)、実施済みの試験と配備状態は[0.6実装記録](mcp-approval-v06-implementation.ja.md)を参照してください。

DB schema 3への更新前に、稼働中の実行がないことを確認してサービスを停止し、設定・状態・実行ファイルを保存してください。更新後に新しい要求を受理したDBを、旧schema対応バイナリへ戻さないでください。
