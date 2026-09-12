# v2実装・受入記録

- [v2 interaction relay / 対話要求の取得・回答API](interaction-api.ja.md)：2026-09-13の追加実装。常駐未反映、Gateway UI・結合受入は未完了。

2026-09-11。契約0.2の実装を追加し、ローカル自動試験と実Codex試験を実施。**本番受入は未完了**。常駐サービスには同日21:05 JSTに反映済み。設定は継承し、OpenAI互換APIと拡張APIを標準で同時提供する。配備確認は[常駐運用記録](server-operations.md)を参照。v2は既定で有効、Capabilityの`implementation_status`は`acceptance_pending`。

規範: [ワーク・成果物・回答復旧API](workspace-artifact-api-v2.ja.md)。設定と管理CLIは[運用手順](v2-operations.ja.md)。内部構造は[実装設計](v2-system-design.ja.md)。

| 項目 | 実装・確認結果 |
| --- | --- |
| SQLite・ID・復元世代 | 同時所有拒否、transaction rollback、通常再起動でID維持、正式復元で世代更新を確認 |
| 会話・ワーク・永続実行・停止 | 受付前の取消予約、同じ会話／共有ワークの競合、明示停止と同一キー再送を確認 |
| UNKNOWNの管理解除 | 状態をunknownのまま保持、監査とrevision照合、旧会話隔離、元要求の再送防止を確認 |
| 成果物 | 不変コピー、同じ操作の原本再取得禁止、symlink・hardlink・FIFO・path traversal拒否、予約後の権限撤回を確認 |
| 保存復旧 | 本体公開とDB確定の間の障害、コピー予約中の再起動、同じ保存バイト列からの復旧を確認 |
| 本体取得・保持 | Range/416/Content-Digest、リースとGC、上限・有限期限、読取中の参照保持を実装。Range応答の実バイト列とdigest一致を確認 |
| 回答 | 実行状態と保存状態を分離。ready後の再取得と再起動後の一致を実Codexで確認 |
| v1互換・移行 | 元JSONLの退避・ハッシュ、SQLiteへの一度だけの移行、既存要求キー・失敗Response IDの採番維持、旧UNKNOWN占有を確認 |
| 正式復元 | 部分stagingとマーカーからの再開、同じ復元IDの維持、解除の再送、Codex履歴を含む実接続復旧を確認 |
| 容量 | 256 MiB×2の同時保存に成功。ローカル測定29.342秒。ゼロ埋めの疎ファイルを読み、保存先は全バイトを書き込んだ試験 |

## 実行した試験

- `cargo test --offline --all-targets`: 106件成功。容量測定用の1件は通常実行ではignored。
- `cargo clippy --offline --all-targets -- -D warnings`: 成功。
- `cargo test --offline --test v2_store measure_two_maximum_artifact_copies -- --ignored --nocapture`: 1件成功、上記の容量測定。
- `python3 scripts/live_v2_smoke.py`: 隔離したProxyとCodex CLI 0.153.4で以下を確認。常駐サービスは操作していない。

1. Lunaによるファイル生成と専用ツール登録、本体とハッシュの一致。
2. Proxy再起動後に同じ回答・成果物を再取得。
3. 同じ会話でLunaからTerraへ変更し、直前の会話内容を継続。
4. Thread再開・モデル変更後に同じ専用ツールで成果物を登録。
5. 初回Turnの明示中断を実際に確認し、同じ会話で次の依頼を実行。
6. 正式バックアップ／復元、世代更新、解除の同一結果再送、保存物一致、元の会話での継続。

実接続試験の再実行で、再起動後のTerraが試験専用の120秒期限を超えた回もあった。その時点はin_progressであり成功に数えていない。通常設定のidle timeout 600秒に合わせて再確認したところ、上流完了後のSQLite書込み競合を検出した。v2の読取・更新transactionをBEGIN IMMEDIATEにしてv1の書込みと直列化し、更新昇格競合を避ける修正を追加した。実行workerが消失した場合の監督・同一Turn照合と、HTTP futureが消失した受付済み要求の起動も追加した。モデルの遅延だけとは判断していない。回帰試験は修正前に503で失敗、修正後に成功することを確認し、修正後の実Codex試験6項目もすべて成功した。

## 本番受入として残る確認

- Gatewayとの通信切断・再起動・世代照合・Discord配信結果不明を含む障害時の結合試験。生成画像の正常系配信・表示は2026-09-13に確認済み（末尾参照）。
- 別ホスト／低速LANでのダウンロード切断・Range再開。
- 実行2件、コピー2件、配信4件を同時に流した制御API応答・メモリ・ディスク・長時間負荷の測定。
- 実ファイルシステムの容量枯渇、fsync失敗、任意の各永続化境界での強制終了を網羅する障害注入。現在の試験は代表境界を再現したもの。
- 候補の容量・保持既定値の最終合意。他のモデル／Codex版の受入。未確認モデルを`registration_models`へ追加しない。

上の未確認項目を「異常なし」や「製品として保証済み」と読み替えない。未完成の試作として品質要件を縮小するのではなく、契約の受入条件を満たした証拠を追加して本番受入を判断する。

## 2026-09-12：生成画像の自動登録

[生成画像API契約](generated-image-api.ja.md)に対応。対応版で受理したResponseについて、記録済みThread/Turnの完全な履歴から画像を不変成果物へ登録する。回答保存、登録状況、Gateway配信を分離し、SSE欠落・登録結果記録前の停止・予約結果不明を扱う。

- `cargo test --locked --all-targets --quiet`：116件成功、2件ignored（容量測定と実画像fixture試験）。
- `cargo clippy --locked --all-targets -- -D warnings`：成功。
- 新規9件の回帰試験で、PNGのHTTP取得、元バイト列・MIME、画像なし、部分失敗、別Turn・不完全一覧の拒否、件数・サイズ制限、再起動、予約済みUNKNOWNの再コピー禁止、保持期限・アクセス撤回を確認。
- 実App Serverから再取得した画像項目のPNG 3,400,858 bytesを、隔離したストアへ保存して元バイト列との一致を確認。専用fixture試験はignoredを明示解除して成功。

常駐サービスへの配備とGatewayによるDiscord実表示は、この追加機能の実装試験とは別の確認項目。

`python3 scripts/live_v2_images.py` も成功。隔離したProxyと実Codex Lunaで新規画像生成を実行し、返された3画像（1,045,451／1,028,899／998,828 bytes）の自動登録、PNGのHTTP取得、サイズ・SHA-256一致、Proxy再起動後の同じ画像一覧・成果物ID・本体の再取得を確認した。常駐サービスとDiscordには操作していない。

最初の隔離試験は親サンドボックス内で起動したため、Codexのスキル読取りが `codex-bwrap-synthetic-mount-targets-1000/lock: Read-only file system` で停止し、画像生成には到達しなかった。これは成功に数えていない。実行許可を得て親サンドボックス外から同じ隔離試験を起動し、上記を確認した。試験はモデル実行を伴うため、要求結果不明時の自動再実行は行わない。

この生成画像対応は2026-09-12 21:23 JSTに常駐Proxyへ反映済み。配備後の確認は[常駐運用記録](server-operations.md)を参照。Discord実表示の確認結果は次節を参照。

## 2026-09-13：Gateway配信・利用者による画像表示確認

Gateway側の改定と常駐サービスへの反映が完了し、利用者から実Discordで画像表示のユーザーテストに成功したとの報告を受けた。生成画像の自動登録からGatewayによる添付・表示までの正常系は確認済みとする。

Gatewayの[実装・受入記録](../../codex-hoshikage-gateway/docs/implementation-status.ja.md)にも、実Codex生成PNGを常駐Proxy経由でDiscordへ添付し、CDNから再取得したSHA-256・PNGデコード・画像内容を確認した結果と、2026-09-13のサービス反映・利用者確認が記録されている。配信契約はGatewayの[生成画像配信仕様](../../codex-hoshikage-gateway/docs/generated-image-delivery-contract.ja.md)を参照する。

今回の確認で画像表示の対応待ちは解消した。上記「本番受入として残る確認」の障害・容量・負荷などをすべて完了した扱いにはせず、Capabilityの `acceptance_pending` は維持する。

## 2026-09-13：対話待機・互換入力・障害境界の追加確認

対話不能時の待機防止、承認失効、未対応ツール指定の明示拒否を追加した。実プロセス強制終了、書込失敗、HTTPボディ切断・Range再開の検証範囲と配備状況は[追加受入記録](proxy-hardening-2026-09-13.ja.md)を参照する。上記の別ホスト実通信・長時間負荷・Gateway障害結合を全件完了した扱いにはしない。
