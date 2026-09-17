# API 0.6 実装単位・内部設計と進捗

2026-09-17。利用者からGatewayのC06-01/02解消・接続Goと実装指示を受領した。[接続契約](mcp-approval-api-v06.ja.md)を基準に、以下の機能単位で設計完成後に実装する。全ツール意味解析は共通機能の開始条件にしない。各単位の試験成功をAPI全体の受入や配備完了と読み替えない。

> 最新状況：T01〜T06のProxy実装・単独受入は完了。以下の途中記録にある未実装・未公開の記述は、その時点の履歴です。最終結果は末尾を参照してください。Discordを含む統合試験は利用者の指定によりGateway側が担当します。

## T01 通信の資源境界・取消安全性（内部設計完成）

カタログ詳細設計の通信部分を実装する。既存APIの承認条件は変更しない。

- JSONLは改行を除くフレーム長256MiB以下。有限バッファで逐次読み、超過・途中EOF・I/O異常をtransport failureとする。上限ちょうどは受信可能。生フレームはエラー通知・ログに含めない。
- envelopeのresultは借用RawValueで受ける。RPCのmethodを先に識別し、server requestとclient responseのID空間を分離する。明示null結果は成功。
- pendingは短時間の同期Mutexを持つmap。登録guardをfutureが所有し、Dropで同期回収する。lock中のawait・JSON parse・I/Oは禁止。正常返信・timeout・外部cancel・書込失敗・transport終了で回収し、遅着を別要求へ割り当てない。
- JSONL書込の途中でfutureが取り消されると、その後のRPCを連結して破損し得る。書込開始時guardを設け、未完了でDropされたらtransportを閉じる。送信未完了でも再送せず、既存の障害復旧へ渡す。stdinロック待ちの取消は通信を閉じない。
- カタログ応答はresultの生UTF-8長8MiB以内を確認してからValue化する。IDとresultの記述順には依存しない。カタログだけにノード262144、深さ64、復号文字列合計8MiBのvisitor制限を適用する。重複object keyはcatalog_invalid。その他RPCは従来のValue処理とし、カタログ制限を画像や回答へ波及させない。
- 超過・不正カタログは対象RPCの失敗。フレーム超過は境界を回復できないため通信全体の失敗。カタログ正常受信だけで定義評価済みにはしない。

試験：小さい注入上限でフレーム境界・部分read・EOF、取消guard回収、送信guardの正常/異常、result null・server/client同ID、カタログ1MiB超・8MiB境界・ノード/深さ/文字列・重複key、通常の大きな結果。既存runtime・v2承認・画像試験を回帰実施する。実Codex/Discord受入とは区別する。

## 後続単位と依存

1. T02：Thread/runtime/設定世代に束縛したカタログ全件取得、共有worker・期限・失敗状態。既存の3 rendererだけへの抽出を新機能の正本にしない。
2. T03：完全な実引数の表示、公開/本人向け判定、不変ページ・照合token・単発返信。未評価の意味は許可を妨げず、公開区分不明は本人向けにする。
3. T04：永続policy binding、store migration、60秒準備、送信意思、停止・再起動復旧。会話を維持した設定隔離の検証を完了してから実装する。
4. T05：選択されたpolicyだけの適格性・禁止・grant適用と失効、競合試験。標準APIへの波及を検証する。
5. T06：機能統合後にcapabilityを公開し、Gatewayとの接続・実Codex・実Discord受入。常駐反映は受入と配備指示に従う。

後続の型・DB・状態遷移の内部詳細をこの文書へ完成させてから各コードへ進む。API未完成の間にmcp_approval_v06.enabled=trueを返さない。

## T01 実装・検証記録（2026-09-17）

T01の内部設計を先に記載した後、`src/runtime.rs`と`src/runtime/wire.rs`へ実装した。外側のtimeout/future破棄でpending登録が残る経路と、途中書込取消後に後続JSONを送信できる経路を是正した。カタログresultは8MiB検査後に制限付きで解析する。従来presentation側の1MiB制限・同期取得はT02の置換対象で、T01だけでカタログ障害全体が解消したとは扱わない。

| 検証 | 結果 |
| --- | --- |
| `cargo test --lib runtime::` | 6成功：フレーム境界、カタログbyte/node/depth/重複key、RPC待機取消、途中送信取消、stdin待機取消 |
| `cargo test --test runtime_integration --test v2_presentations --test v2_mcp_grants --test v2_images --test mcp_reload` | 45成功、実環境依存2件ignored。RPC遅着、8MiB境界、8MiB超の通常応答、null結果、双方向ID衝突、承認/画像/設定更新を確認 |
| `cargo clippy --all-targets -- -D warnings` | 成功 |
| `cargo fmt --check`、`git diff --check` | 成功 |
| API JSON例と容量予約の文書検査 | 成功。製品APIの実装試験とは別 |

新規の試験はfake App Serverと隔離したecho子プロセスを使用した。実Codex/Discord接続、256MiBの実画像経路、RSS受入は未実施。API 0.6 capability・新エンドポイント動作・永続policy準備は未実装であり、T02〜T06が残る。常駐設定・サービス・Gatewayコードは変更していない。

## T04 設定分離の追加実証計画（製品コード着手前）

導入版0.153.4の `resume_running_thread` は、購読中のThreadではconfig overrideを無視する。一方、購読なし・Idle・実行中でないThreadにconfigを指定すると、shutdown完了を待ち、同じThreadの保存履歴から設定を読み直す。`thread/unsubscribe`単独は即時unloadを保証しない。`config:{}`も明示した再開で同じ冷再開条件へ入る。

隔離したCODEX_HOMEと無害なローカルMCPを使用し、AI Turnを起動せず、thread/start→unsubscribe→同じthread/resume(config指定)でMCP定義と返却approvalPolicy/reviewerが切り替わること、別Threadは変わらないことを確認する。configなしの再開を保証根拠にしない。結果は観測事実と保証範囲を分けて残し、これだけで実Notion guard受入完了とはしない。

## T02 カタログ収集器・共有キャッシュの内部設計（完成）

`v2::catalog`へ旧presentationと独立した収集器を置く。キーはinstance/recovery/runtime/thread/configの5要素。実runtimeのIDは生成時のUUIDで、同じPID再利用を同一としない。生成済みruntime IDとthread IDをRPC先に束縛する。

全ページの検査が完了するまでsnapshotを公開しない。8MiB/page、32MiB/取得、64page、4096tool、cursor8192bytes・server/tool各1024bytes。server重複、tool定義のname不一致、cursor循環/空/不正型、data/nextCursor/runtimeStatus/authStatusの欠落・不正は理由付き失敗。定義全体のハッシュとserver状態を保持し、生descriptionをキャッシュしない。取得ページの定義hashは既存の正規化fingerprintを使い、ツール名だけで評価済みとしない。

Managerは最大32key、runtimeあたり同時2worker、登録から60秒（queue込み）、RPC30秒。keyごとにwatch受信口と一つのworkerを共有する。GET側の待機250msとworker寿命を分離する。readyは30秒、failedは2秒で再取得可能。同一内容の正常更新ではepoch維持、失敗/明示無効化では新epoch。古いworker結果の採用はkeyとticketで照合し、削除・置換後の遅着を破棄する。

保持量は全String capacityとmap/Arc等の保守的予約額で計上し32MiBを超えない。キーだけの予約も含める。全使用中keyを維持できない場合はcatalog_capacityで拒否し、黙って削除しない。呼出し側が使用を終えたkeyをreleaseした場合だけ回収する。設定世代変更・runtime終了・復元・server通知欠落の利用側通知はinvalidateで即時利用不可にする。

検査：全ページ成功/途中失敗/重複/循環、全サーバー保持、個別server障害、1MiB超、同key100要求でworker1、別key枠2、期限・TTL・失敗共有、release/invalidateと遅着、容量超過を試験する。表示とgrantへの接続はT03/T05で行い、collector完成だけをUI問題解消と扱わない。

## T03 汎用表示の内部設計（完成）

引数の完全性と操作の意味評価を分離する。表示組立ては副作用のない専用moduleで行う。入れ子をJSON Pointer（`~0`/`~1`）・原キー・型・配列indexへ対応付け、object/arrayの空/非空も項目として表す。文字列値はJSON表記（引用符/escapeあり）で全文を分割する。数値の原表記・精度を保ち、原引数にない値は足さない。部分fieldには分割位置を示し、復元順を固定する。

1ページはtitle/label/value/limitations/omissionsを含め4800 UTF-16 units、24fields、応答64KiB。内容の1fieldは1800 UTF-16 unitsを基本上限にし、code point途中では切らない。総表示JSON262144bytes、最大64pages。元会話は1ページまで、超過は本人向け全ページへ進む。表示しきれなければ許可不可。構造・数値精度・重複キーの検証はT01 reader側で行い、call観測時に全引数の完全性を確定する。

上流定義未評価の場合はraw rendererと固定の未評価説明を返す。既存公開規則に一致する通常操作は元会話表示を維持し、未知privacyは本人向けでprivacy_unclassified。本人向けには伏字ではなく完全な入力値を提示する。操作表示の生成に外部送信はしない。

ページ本文はDBへ保存しない。既存callキャッシュから決定的に再生成する。DBにはaudience別の版数、presentation ID、全表示digest、scope、失効時刻、fingerprintとページtokenの照合メタデータだけを保存する。全表示・scope/binding・期限をprocess固有HMACで拘束し、再起動後の旧tokenは使えない。audience別4版、初回期限を延長せず、GETで版を増やすのは内容・根拠変更時だけ。全ページtokenを送信意思保存transaction内で照合する。

拒否はページ・binding・catalog取得失敗から独立させる。未知ツールを許可済み意味として登録せず、禁止policy検査と実call/full arguments/全ページだけで単発の可否を判定する。依頼中許可はT05の評価済み判定を別に要求する。

### T04 隔離実Codexの観測結果

`python3 scripts/live_v06_config_isolation.py`と同内容の隔離試験を導入版0.153.4で実施した。AI未開始の空Threadは保存rolloutがなくresumeできなかったため、正式な`thread/inject_items`で無害なローカル履歴マーカーだけを追加した。モデル呼出し・外部MCP・本番サービスへの送信は0件。

同じThread IDを維持し、購読解除後の明示configでMCPツールalpha→gamma、さらにconfig={}で当該ツールなしへ切替成功。並列の別Threadのbetaは不変。approvalPolicy/reviewerの返却値も指定と一致した。証拠 `/tmp/proxy-v06-config-hhu0sa91/result.json`。これは同一Threadの冷再開・設定分離の実証であり、Notion guardの実操作やAPI全体の受入ではない。

T02収集器は全サーバーの定義hash・状態、共有worker/容量/取消/世代を実装し、単体6件成功。T03の型付き全文投影を実装中。まだ新APIへ接続せず、旧presentationの暫定的な上限変更は行っていない。

### T03 原引数の数値・重複キー

実callのitem/startedはparamsをRawValueから再帰的に読む。objectの重複キーはJSON escape後の同一性でも拒否し、object/arrayの要素を独立に解析する。serde_jsonのarbitrary_precisionを有効化し、Numberを数値文字列から直接生成する。大整数・長い小数をf64へ通さない。`$serde_json::private::Number`という利用者のobjectキーもobjectとして保持する。通常Valueの汎用deserializeによる特別キー解釈をcallの原引数には使用しない。

解析不能な実callはargumentsを利用不可としてマークし、キャッシュの同じcallを無効化する。不正入力を別の値に修復して承認に用いない。未知の別イベントは従来の取扱いを維持する。

## T04 永続準備状態の内部設計（状態機械部分完成）

公開approval_policyと非公開の準備台帳を分ける。公開部は接続契約の完全型。非公開部にはboot_id、CLOCK_BOOTTIME開始/期限、設定RPC送信意思、runtime ID、設定根拠hashを記録する。UTCの受付時刻と60秒期限はimmutable。時計後退/boot不一致では残り時間を作り直さず期限切れとする。壁時計と単調時計の早い方で期限切れを判定する。

状態の更新は既存store transaction内から呼べる副作用なし関数へ集約する。設定RPC送信意思→応答確認→ready→Turn送信意思→開始確認を分離する。stop_requestedを確認してからTurn送信意思を同transaction内で保存する。設定のみ不明ならnot_startedを維持し占有保持、Turn送信意思があればunknownを維持する。設定不明を確定した要求は後で設定が正常と分かっても再実行しない。

停止はfailedの元理由を消さない。設定変更の未送信が証明できる場合だけ即時取消/解放できる。送信済みで結果不明の場合は停止意思と隔離待ちを保持し、外部の隔離確認を成功と偽らない。再起動の分類は未送信準備の再開・設定結果照合・Turn結果照合・終端の4種で、新しいbindingや期限を発行しない。

この節は準備状態機械の実装開始条件。Thread設定変更の製品接続・ストア移行・隔離確認のコードは別途、実証済み冷再開手順と連携設計を完成させてから接続する。

### T03/T05 初期評価済み範囲

汎用単発表示は全ての対応付け可能な実callを対象とする。公開説明と依頼中許可の初期評価済み範囲は、既存レビュー済みのPlaywright find（text/regex択一）、navigate（http/https URLのみ）、tabs（listのみ）とする。観測した定義全体のSHA-256へ固定し、定義不一致や未知の引数は未評価として完全な本人向け単発表示を維持する。browser_evaluate/unsafe等をこの範囲へ推測で加えない。

公開判定にはPR01〜11のローカル検査を併用する。公開向けの完全raw fieldsに固定操作説明を追加し、全文が1ページを超えた場合はdisplay_too_largeで本人向けへ進む。カタログが取得中/失敗でも単発本人向けを禁止しない。policy未選択は評価済み定義があっても依頼中許可なし。

### T04 Thread設定の製品連携設計

同じconversationの既存Threadは、対象実行の占有を保持して購読解除し、configを必ず明示して冷再開する。config省略による実行中設定の再利用は行わない。新規Threadも同じ設定証拠を取得してからTurnを開始する。単発表示が必要とするnative-call-ID形式は当該Threadの`features.tool_call_mcp_elicitation=false`で選択し、未選択の他Threadへ上書きしない。

guard選択では、信頼されたcodex_appsの正確なNotion update-pageのconnector ID・link ID・完全定義を取得して、対象toolのapproval_modeだけpromptへ重ねる。他toolの常時許可・明示無効を保持する。既存のapp/link reviewerがuserでなければ別toolまでreviewerを変えず、policy_configuration_conflictで準備を止める。モデル自動審査/managed app制約を確認できない場合も同じ失敗とする。configRequirements/readはmanaged Appsの全項目を公開しないため、requirementsが非nullの環境を設定echoだけで検証済みとしない。対応外条件を明示して停止する。

設定の基本値はcwdを指定したconfig/readを使用するが、これはThreadの読戻しと偽らない。冷再開の上流版固定・購読者なし・idle・戻り値とカタログの一致を根拠にする。設定fingerprintは秘密を含む原値を外へ出さず内部hashのみ保持。設定世代が途中で変われば旧根拠でTurnを開始しない。

準備中のmutation intentは前後のDB transactionで保存し、RPC中にDBロックを保持しない。複数段階がある場合も各送信の未確認状態を区別する。停止は未送信段階から次mutation/Turnを禁止する。設定応答不明では送信を再試行せず占有保持。返却を確認できた設定の下でAI未開始が確実なら準備失敗/取消として解放できるが、guard解除の責任はconversation側に残す。次の旧profile/非選択Runでも解除完了を確認するまで開始しない。

生設定・本文は準備台帳/公開Responseへ保存しない。store schema3へ移行し、schema2は同transactionで更新、未知版と旧binaryは拒否する。既存行は旧profileのまま、新規bindingは作らない。起動時は準備記録がある行だけT04状態機械で復旧分類し、従来の一括UNKNOWN変換を適用しない。旧Threadへの標準API実行がguard設定を引き継がないよう、runtime側にもbindingで送信を制限する境界を置く。

### T05 評価関数の内部設計（完成）

カタログ状態・実引数・準備証拠を入力とする副作用のない評価関数へ分離する。公開区分と依頼中許可を同じ判定にしない。個人情報を含む検索は本人向け表示になるが、認証情報を送信する操作とは区別する。資格情報の文字列検査は公開検査と同じ正規化・最大4回の復号を使い、検査不能時は依頼中許可なし。元の引数は変更しない。

初期評価範囲は上記3定義に限定し、tabsのnew/select/closeは未評価として単発確認を維持する。契約JSONにある複数操作の配列は型の例で、実装済みの宣言として使わない。既知ツールでも定義不一致はdefinition_changed、引数不一致はtool_not_evaluated。カタログ取得中・失敗はcatalog_unavailableとし、「未対応」へ置き換えない。

未選択ではtool_policy=null。選択時は全フィールドを返し、追加禁止の判定と依頼中許可の適格性を分離する。evaluated-turnは未知操作を禁止せず、完全な引数の単発承認を維持する。Notion guardは準備時の対象完全定義hash・設定世代・runtime一致を要求し、根拠が失われた場合はpolicy_check_unavailable。対象のupdate_contentは常にpolicy_denied、未知commandは検査不能。他toolへ禁止を波及させない。新profileの製品受付は、この評価関数だけでは有効化しない。

Notion guardの非禁止判定はcommandの文字列だけで行わない。対象ツールの未知引数、別commandに混入したcontent_updates、必須入力の欠落・型違いも検査不能にする。これは当該guardの禁止検査を確実にする条件であり、非選択の単発承認へ持ち込まない。内容の意味評価・移動/共有作用の判定とは別で、この検査を通っても依頼中許可の対象にはならない。

### T04 グローバルMCP更新との排他

Threadのbinding取得とグローバルMCP reloadを同じ非同期mutexで直列化する。binding保持中は設定更新の必要性を読取りだけで判定し、変更ありなら生成設定を書き換える前に処理を止める。変更なしの既存実行は継続できる。直接のreload要求もbinding保持中は拒否する。binding解除や停止RPCはreload待ちに巻き込まない。取消でreload応答が不明になった場合は、従来の未確認状態を保持し、次の設定準備で確認するまで新しいbindingを確定しない。

### T05 grant保存・適用の内部設計（完成）

新profileのgrantは旧profileと判定経路を分ける。作成は本人による明示的なgrant_scope=turn_toolと全ページ確認が揃った時だけ。作成時の新scope、execution_policy、grant_policyを不変で保存する。1件49152bytes・Responseあたり16件・一覧1MiBを保存前に予約検査する。grant期限は作成時から10分、後続利用で延長しない。

後続callは実引数、完全性、現在scope、設定・定義世代、選択policyの禁止検査・適格性を送信意思transaction内で再評価する。自動返信は内部選択したgrant IDを持つ経路だけでページtokenを省略でき、外部HTTP入力から同じ扱いへ入れない。scopeはcall IDを含まないが、各callの実引数照合は独立に必須。通常の単発許可からgrantを生成しない。

元のnative対応情報は内部記録として維持し、受理時メタデータは別フィールドに保存して公開operationへ投影する。そこへ生引数を保存しない。拒否時は表示評価が失敗しても拒否を送信できる。カタログ更新中はgrantを削除せずavailabilityを待機にする。更新成功・同一epochなら同じgrantを再評価でき、失敗・定義変更・設定変更・停止・追加発言では使わない。

## 追加実装・検証記録（2026-09-17、製品受付の接続前）

T02の収集器、T03の全文ページ・公開区分・単発照合、T04の状態機械・schema2→3移行・準備停止／再起動分類、T05の定義束縛評価・明示grant保存／適用をコードへ実装した。実行中のbindingとグローバルreloadの競合を拒否し、評価済み表示には検索語・正規表現・アクセス先URLの日本語ラベルを付ける。Notion guardのreviewer判定ではnullによる上位設定の継承も確認する。

`cargo test --lib --test v2_approval_v06 --test v2_store --test v2_mcp_grants --test v2_interactions --test v2_presentations --test runtime_integration --test mcp_reload`は153成功、実環境／容量測定の2件ignored。新profileの試験は準備済み記録を試験内で構築し、偽App Serverから実際の評価対象3定義を取得している。HTTP受付から実Codex実行までの試験ではない。後続修正については対象試験とClippyを再実行する。

追検証：`v2_approval_v06`6件、`v2_mcp_grants`14件、`v2_store`26件（容量測定1件ignored）、`approval_config`4件が成功。tool_policyの`version`とgrant_policyの`policy_version`を正式JSON例と照合した。最終Clippy（全target、警告をエラー化）、fmt、diff検査も成功。

**未完了**：製品の受付validatorと冪等要求の正規化、engineへの設定準備／Turn送信意思の接続、設定不明時の隔離照合、全経路の容量予約、capability公開、実Codex／Gateway／Discord受入。新profileはまだ製品受付で許可せず、capabilityも公開していない。既存の旧profileのカタログ表示経路を、この新実装の試験だけで改修完了とは扱わない。常駐環境・Gatewayコード・リモートリポジトリは変更していない。

## T04/T06 接続の最終内部設計（実装前確定）

受付では0.6 profileとpolicyを厳密に検証し、policyの省略とnullを同一の冪等要求へ正規化する。受付transactionでbindingと60秒期限を発行する。準備中はdispatching/not_startedとし、設定変更直前・ready公開・Turn送信意思を停止と同じtransactionで確定する。

新Threadのguard対象はprocessカタログから正確な定義を先に得て、最初のthread/startに設定を含める。既存Threadはidleを確認してunsubscribe→明示config付きresume。Turnを一度も送信しておらず上流履歴も空と確認できるThreadに限り、保存rolloutがない場合は新しい上流Threadを作る。Proxy会話ID・ワークは維持し、利用者の履歴を破棄／分岐しない。送信意思が一度でもあるThreadはこの経路へ入れない。

設定変更の所有権はconversationへ記録する。次のRunが旧profile／未選択でも、以前の設定所有権がある会話は明示configで準備し直す。runtime内の所有権移譲は旧bindingと無通信中を比較して行い、解除と再取得の隙間を作らない。設定の原値は永続化せず、同一プロセスのランダムHMAC鍵による根拠だけを保持する。

設定RPCの応答が不明なら再送しない。Turn未送信を維持して占有を保持する。後の隔離照合では、旧runtimeが生存しないこと、または既知Threadのidle・元RPC待機の終了と明示設定の読込みを確認する。元要求は失敗／取消として確定し、入力を実行しない。準備未送信の再起動だけ同じbinding・期限で再開する。

製品API応答は最後のJSON変換後に契約のサイズを検査する。保存前予約はResponse／interaction196608bytes、policy4096bytes、grant49152bytesを適用し、公開時の追加項目・日時変換を含めた枠を残す。0.6は同時pending16、履歴256。拒否・停止は表示やカタログの成功から独立させる。

#### 前回設定の解除を含む送信境界の補足

前回の設定を解除する `thread/unsubscribe` / `thread/resume` も、今回の準備期限内で行う設定変更である。解除の前に同じ永続準備記録へ送信意思と上流プロセス識別（PIDと起動時刻）を記録し、解除確認後に次の設定送信へ進む。解除結果が不明なら今回のTurnは送らず占有を保持する。新規作成後に一度もTurn送信意思を記録していない会話だけを空会話として扱い、履歴のある会話・旧形式で判定情報のない会話は作り直さない。

設定の所有権は実行終了後も会話に保持するが、終了済みの所有権はプロセス全体の設定更新を妨げない。実行中・設定送信結果不明の所有権だけが設定更新を保留する。終了済みの会話への無指定の直接実行は引き続き防ぎ、次の正式な実行経路で前回設定を解除する。

#### T06 隔離した接続受入の実施方式

実Codex/実モデルの試験は別CODEX_HOME・別DB・別ワークスペースで行う。ローカルMCP fixtureは固定文字列を返し、ブラウザーや外部サービスを変更しない。製品のHTTP受付から準備、実イベントのcall ID照合、操作表示、明示許可、同一Turn内の次呼出し、同一会話の次依頼までを通す。既存の手動シードだけの単体試験とは分けて記録する。Gateway型との接続照合には製品HTTPで返したJSONを用いる。Discordの本人確認と画面押下はGatewayの受入範囲であり、HTTP照合をDiscord実機受入と呼ばない。

実Codex接続で、`thread/start`が生成済み設定へ作業ディレクトリの`projects.<path>.trust_level`を保存することを確認した。この自動保存をMCP承認設定の変更とは扱わない。設定世代の入力から当該trust_levelだけを除外し、それ以外の既知・未知の設定値は引き続き照合する。作業ディレクトリのアクセス制限は別途cwd policyで検証する。MCP設定・Apps設定・承認設定の変更を除外してはならない。

上流がTurn開始RPCの応答直後にcallイベントを送る場合、DBのTurn ID確定より先にイベント監視へ到達し得る。送信意思を記録済みのdispatching Responseも、同じThreadの観測キャッシュの容量・profile判定対象にする。ただし承認の送信は、開始応答で確定したTurn IDとの一致後に限る。Notion制限を選んだ実行では、上流のMCP承認形式を検出したのに実callへ対応付けられない要求を旧単発経路で許可しない。検査不能として拒否し、元実行を停止する。

実Apps接続で、Thread IDなしの`mcpServerStatus/list`はruntimeStatusを返さず、tools/authだけを返すことを確認した。準備用のプロセスカタログに限り、欠落/nullのruntimeStatusをnotStartedとして扱う。これは接続完了の証拠にしない。準備では対象定義と認証情報の存在を検査し、設定適用後にThread ID付きカタログで接続済み・同じ対象定義を確認して初めてreadyにする。Thread ID付き取得で状態が欠けた場合は従来どおり検査失敗とする。

60秒の期限監視は設定RPCだけでなく、モデル解決とprovider枠待ちを含めた実行worker全体へ掛ける。期限到達時、永続Turn送信意思がまだなければ準備futureを取り消し、設定送信記録に従ってfailedかunknownへ確定する。送信意思以降は準備timeoutでAI実行を失敗扱いにせず、既存の実行監視へ任せる。

Thread作成直後のMCP状態がstarting/notStartedの間は、準備失敗と確定せず、同じ60秒期限内で2秒間隔のカタログ再取得を行う。期限を延ばさず、停止意思も毎回確認する。connectedになってから対象定義を照合する。認証要求・失敗・無効化・定義の不一致は待機で隠さず準備失敗にする。


## Proxy実装・単独受入の完了記録（2026-09-17）

T01〜T06のProxy担当を実装し、製品HTTP受付へ接続した。source-conversation-v3をResponse単位で受け付け、mcp_approval_v06を公開する。表示・単発許可は旧スイッチOFFでも利用できる。選択policy、準備期限、設定送信意思、Turn送信意思、停止・再起動照合を永続状態へ接続した。旧profileとOpenAI互換APIを保持する。

- 全件カタログ取得と全引数表示、公開/本人向け・長文ページ、HMAC照合、拒否独立、許可の明示作成・照会・取消・失効を実装。
- HTTP試験で、ポリシー付き→未選択→旧形式の同一会話継続、null/省略の冪等性、設定準備中の停止、設定不明時の占有、上流プロセス終了後の隔離確定、元要求の再送禁止を確認。
- 呼出しごとの照合と、明示許可1回で5呼出し、スイッチOFFで5回とも単発確認を確認。Notionの未対応付け承認が旧入力経路を迂回しないことを確認。
- 実Codex 0.153.4・gpt-5.6-luna・隔離したローカル読み取り専用MCPで、製品HTTP受付から承認・実行完了まで成功。同じ会話の次の依頼はポリシー未選択で、前回の許可を使わず単発確認した。
- 実Apps 272定義とローカルMCP1定義を取得する構成でも、Notion guardを準備して上記5回＋次依頼1回が成功。Notionの実ページは変更していない。Notion部分置換の拒否は実定義に束縛した判定試験で確認した。
- 実Codex試験で見つかった、信頼設定の自動保存による世代誤判定、ThreadなしのruntimeStatus欠落、Thread作成直後のstarting待機を修正。Gateway実装での照合で公開audienceの必須nullを補完した。
- 実HTTPのcapability・presentation・Response・grant JSONを、Gatewayの現行ソース`mcp_v06`で検証・描画し成功。JSON例のProxy/Gateway一致も確認。

| 最終検査 | 結果 |
| --- | --- |
| `cargo test --all-targets --quiet` | 成功。既存の明示実行専用ignored試験は別扱い。I/O障害・回帰試験も通過 |
| `cargo clippy --all-targets -- -D warnings` | 成功 |
| `cargo fmt --check` / `git diff --check` | 成功 |
| `cargo build --release --bin codex-hoshikage-proxy` | 成功 |
| `CODEX_TEST_AUTH=… cargo test --test live_v06_http -- --ignored --nocapture` | 実Codex・基本policy・同一会話継続成功 |
| 上記に`CODEX_TEST_APPS=1`を指定 | 実Apps・Notion guard準備・未選択への切替成功 |
| Gateway現行型で実HTTP応答を照合 | 成功。Discordの実画面・本人確認・ボタン操作とは別 |

隔離試験の証跡は`/tmp/v06-live-de53da35-ba38-45c9-809a-c69c00c2b36d`。認証コピーは試験終了時に削除した。ローカル証跡は運用機固有であり、永続的な配布資源ではない。試験を再現するコードは`tests/live_v06_http.rs`と`scripts/fixtures/v06_read_mcp.py`に保存した。

### Gatewayへの引渡し

具体契約は`docs/mcp-approval-api-v06.ja.md`、完全JSON例は`docs/mcp-approval/api-v06-examples.json`。Gateway側の統合試験では、本人・会話の照合、公開/本人向け全ページ、単発/依頼中/拒否、停止/Steer、再起動・通信断を確認する。利用者から「統合テストはGateway側でおこないます」と指定されたため、Proxy側からDiscordへの試験投稿やGateway常駐更新は行わない。これはProxyの実装待ちではなく、Gateway担当の統合受入である。


## 常駐反映完了（2026-09-17 19:54 JST）

実装コミット `3a38734` を常駐へ反映し、DB schema 3、API 0.6 capability、既存設定・保存回答の保持、実Codexの正常完了・回答保存・重複抑止を確認した。配備と利用者が許可した旧実行の中断は[運用記録](server-operations.md)の2026-09-17 19:54 JSTの項を参照。Proxy担当分は完了し、Discord統合受入は利用者指定どおりGateway側で行う。

## 確認待ち不具合の修正設計（2026-09-17、実装前）

Gatewayの実測により、前節の単独受入には人間の確認待ち時間とprivate入口の型検証が不足していた。カタログTTL超過を意味未評価へ落とす実装と、private_requiredの必須項目不足をProxyの不具合として修正する。契約第14節の既存型を使う。

- カタログの存在する更新状態を表示判定の前に確認し、更新中／失敗なら再照会可能なunavailableとする。既存表示レコードを書き換えない。公開判定をカタログ取得失敗と混同しない。
- APIのカタログ要求開始エラーを捨てない。許可POSTも再取得する。拒否・既受付要求の照会は更新不要。トランザクション内でも状態を再確認し、更新待ちをturn_grant_ineligibleに置換しない。
- private入口は秘密情報を含まない1ページとして通常の表示ID／token生成経路へ通す。unavailableのexpires_atはnull。理由とdiagnosticを全分岐で一致させる。
- 正常更新は既存カタログepoch維持機構を使い、不要な再表示版消費を防ぐ。35秒・90秒の実時間試験を追加する。

### 確認待ち不具合の検証完了

[原因・契約・試験結果](mcp-approval-review-delay-fix.ja.md)に記録。35秒／90秒の実時間4条件、実Codexの35秒依頼中許可・90秒単発許可、失敗／変更／停止競合、Gateway実型・描画関数による非readyを含む応答検証に成功。全回帰試験とClippyを通過。前回の「完了」判定に人間の確認時間・全状態の型照合が不足していた点を訂正し、今回追加した受入試験を今後の確認条件に含める。
