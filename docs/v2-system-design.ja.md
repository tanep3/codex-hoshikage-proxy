# v2実装設計

> **廃止済みの履歴資料:** 2026-09-23の構造改定により旧`/v2/codex`専用実装は撤去対象である。現行設計は[システム設計書](codex-hoshikage-proxy-system-design.md)と[Codex Native API契約](codex-native-api.ja.md)を参照する。

[契約0.2](workspace-artifact-api-v2.ja.md)に対する実装。公開契約、[運用手順](v2-operations.ja.md)、[受入記録](v2-implementation-status.ja.md)を分けて管理する。

## 責務と保存境界

GatewayはDiscord認可、会話の待機列、配信意思・結果、結果不明の配信隔離を所有する。Proxyは実行要求の永続受付、Codex Thread/Turnと会話・ワークの対応、停止意思、全クライアントのワーク／Provider占有、成果物・確定回答の保存・期限・世代を所有する。

`src/v2/store.rs`が単独所有ロックとSQLite WAL/FULL同期commitを提供する。書込みはBEGIN IMMEDIATEで読取後の更新昇格競合を防ぐ。`records`は会話、ワーク、Response、停止、取消予約、成果物、リース、読取参照、監査を保持する。`operations`は要求キーと正規化要求のfingerprintを保持する。`record_sequences`は一覧の境界を固定する。`legacy_records`は移行後のv1要求・会話対応を保持する。

要求キーの衝突検証と対象ID予約を同じtransactionで行う。予約記録を自動GCしない。v1移行は元JSONLを検証・退避してから一つのtransactionで取り込み、移行済み印を記録する。切断・クラッシュで応答を失っても、同じ要求は保存済みのIDに結び付く。

## 実行と停止

`service.rs`でワーク・Provider占有と回答／入力容量を予約する。`engine.rs`は202を返すHTTP要求から独立して動く。上流送信前にdispatchingを保存し、再起動後のdispatching/startedはunknownにする。acceptedのみ通常再開できる。worker登録で重複起動を抑止し、HTTP future喪失後のacceptedは監督処理が起動する。worker喪失後のdispatching/startedはunknownとして同じTurnを照合し、AIを再送しない。SSEの切断は実行を中断しない。

停止はResponseまたは元要求キーで受理する。先着停止は取消予約を永続化する。acceptedは未送信取消、dispatchingは停止意思を保持、startedは対象Turnへ中断する。中断の送信境界も保存し、不明な送信結果を停止成功に変えない。v1の中断APIでv2 Turnを指定した場合もこの停止意思へ集約する。

`coordination.rs`は同一・包含するワークとProviderの占有をv1/v2で共有する。未送信のv1予約はキャンセル時に解放し、送信済みのUNKNOWNは残す。旧台帳にcwdがないUNKNOWNは全ワークを保守的に占有する。運用者は同じローカル監査経路で解除でき、旧Threadは継続禁止になる。

上流照会で同じThread/Turnの終端を確認すれば占有を解放する。管理解除後も照合し、実行中と判明すれば占有を戻す。解除対象の元要求を再送する経路は作らない。

## 成果物・回答・配信

専用ツール`hoshikage_publish_artifact`と利用者指定pathの取得は同じ保存処理を使う。会話・ワーク・Turnの対応と操作キーはProxyが決める。コピーは別の有界workerで動き、Turnの停止・状態監督をコピー完了待ちで止めない。

Linux `openat2`で登録ルートを基点に開き、symlink・別mount・特殊ファイル・hardlinkを拒否する。開いたルートの実体識別情報を照合する。読取前後のサイズ・mtime・ctime等を比較し、コピー中の更新を検出する。これは外部書込みの原子的snapshot保証ではない。

完成バイト列を同期した後、ハッシュ・サイズ・期限のmanifestを同期し、同一filesystem内で本体をrenameしてからDBをreadyへ確定する。再起動は同じmanifestとバイト列だけで復旧し、元ファイルを読み直して別内容を同じartifact IDにしない。ハッシュ一致だけでは永続化を保証できないため、復旧時も本体・manifest・保存先ディレクトリを同期してからreadyへ戻す。検証後の同期失敗は取得不可の状態を維持し、保存済みmanifestがあるunknown成果物も次回起動時の復旧対象とする。これは元要求や原本コピーの再実行ではない。

実行completedと回答readyは別状態。最終回答の保存失敗・上限超過は実行成功を失敗へ書き換えず、AI再実行の根拠にしない。SSEは永続状態のsnapshotと補助通知を提供し、欠落時はgapを通知する。

ダウンロードは公開保存物の検証後、限定した枠でstream配信する。Rangeは単一範囲、digestは返すバイト列、ETagは保存版の全体に対応する。期限判定・リース・読取参照・GC判定を同じDB境界で処理する。権限撤回は保存版の取得にも適用する。

## 復元と提供判定

バックアップは新規受付を止め、活動実行・UNKNOWN・コピーがない状態でDB、保存物、Codex会話履歴、旧台帳退避をまとめる。原本と認証・設定は含めない。復元はサービス停止下で行い、対象の外にマーカーを置き、新しい世代でrecovery_blockedとする。補助履歴まで同期してから復元完了印を記録する。途中なら同じbundleで再開する。

復元保留の解除は運用者の照合結果とリスク承認を監査記録に残す。同じ解除の再送は同じ監査結果を返す。個別UNKNOWNの占有は独立して残す。任意のディスク全体巻戻しの完全検出は保証しない。

v2は既定で有効。動的ツールadapterは検証済みCLI 0.153.4に固定する。通常再起動はinstance/generationを変えない。v2状態を持つhomeで無効化して実行調停を迂回する起動は拒否する。本番提供の判断は受入記録に従う。

## 生成画像

`images.rs` は終端Responseの記録済みThread/Turnを照会し、完全な画像項目からPNGを保存する。保守処理が有界workerを起動し、HTTP/SSE接続や回答保存に依存しない。Response内に一覧・revision・監視期限を永続化し、操作キーと成果物IDを固定する。再起動時は保存済みmanifestから一覧の登録結果も修復する。パス探索は行わず、内部バイト列保存でも既存の容量・アクセス・保持・公開境界を通す。公開条件は[生成画像API契約](generated-image-api.ja.md)。
