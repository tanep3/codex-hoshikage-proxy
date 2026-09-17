# MCP操作投影・許可ポリシーの詳細設計

> 2026-09-17 責務再整理中：[汎用基盤とポリシー拡張の改訂](mcp-approval-layering-revision.ja.md)を参照。全ツールの意味解析を汎用単発承認の前提とする記述、および追加禁止を標準API全体へ適用する記述は見直し対象。本書をそのまま新規実装へ使わない。既存0.5 wireの後継差分は未確定であり、合意済みとして扱わない。

2026-09-17。全体是正の設計。実装・受入前。対象一覧は[342ツール台帳](mcp-approval-tool-coverage.ja.md)。実行時にツール名や自己申告のdescriptionを読んで許可を推測する設計にはしない。

## 1. 表示と許可の独立性

判定は次の順序で行う。

1. 実call ID・Thread・Turn・Run・利用者・会話を照合する。
2. 対象Threadでの実カタログと評価済み定義・接続を照合する。
3. 全引数の型、必須、排他、既定値、未知キー、複合操作を検証する。
4. 呼出し全体のprivacyを判定する。通常非機密の入力／本文は公開。個人情報・連絡先・DM／メール・認証・任意実行コードは本人限定。どちらとも確定できない場合はprivacy_unclassified。
5. 操作の作用を判定する。外部送信・共有権限変更・削除・任意実行・秘密入力・認証は一回限り。それ以外の評価済み通常操作は依頼中許可可能。
6. 実作用・対象・入力内容・オプション・許可範囲を完全に投影する。秘密を伏せた不完全な表示で直接許可させない。
7. 結果の本文・scope・定義世代・policy世代・actionsをトークンへ拘束する。

本人限定だから必ず危険、公開可能だからターン許可可能、という関係にはしない。メール読取は本人限定でも評価済みreadなら本人限定画面で依頼中許可を選べる。一方、非機密の公開コメントは初回に全文表示できても外部送信なので単発許可だけ。

## 2. 公開／本人限定の具体規則

- Gmailの全操作は呼出し全体を本人限定とする。検索語や件名だけを通常検索rendererで公開しない。
- NotionのCustom Agent sessionに対するメッセージ・イベント内容／検索はDM相当として本人限定にする。公開文書の読取と混同しない。
- family／parental controls／trusted contactは個人・家族・連絡先を扱うので本人限定。操作概要以外を公開しない。
- 宛先、cc/bcc、email、電話番号、住所、個人のuser ID／氏名、参加者、個人の権限対象などの評価済み個人情報フィールドがある場合は本人限定。リポジトリ名・文書ID等のリソース識別子を、根拠なく個人の実名へ変換しない。
- 任意コードの実行引数、Cookie／storage復元、資格作成・環境変数・token・署名URL・秘密入力は本人限定。旧規則のredacted_pathsだけで判定を完結させない。
- 通常の検索語・対象URL・非機密の投稿本文・編集本文・入力値は公開可能。本文の中に秘密・個人情報のパターンを検出した場合は、対象値の部分伏字ではなく呼出し全体を本人限定にする。
- 内容検査はローカルで行い、公開判定のために本文を外部モデルや別ツールへ送信しない。既知のメール／電話／住所／個人番号／資格情報の形式、評価済み個人情報フィールド、入力目的・ツール種別を合わせて判定する。単なるregexだけで全ての実名・独自識別子・偽装情報を検出できると保証しない。
- privacy_unclassifiedを通常公開へ倒さない。どの形式が未分類だったかを秘密の値を含まない診断として記録し、通常の非機密試験がここへ落ちた場合は是正対象として扱う。
- Cursor・ページtokenは資格情報と混同せず、台帳で定義したプロトコル値として非表示にできる。ただし作用・対象を変える任意文字列をcursorへ付け替えて非表示にしない。全実引数は照合fingerprintに含む。

プライバシー判定の誤検出率・検出漏れは受入で評価する。未分類を安易にすべて公開／すべて本人限定として完成にしない。本人限定への移動ボタンを操作の承認ボタンとは扱わない。

## 3. ツール別投影の登録形式

実装するRegistryの各エントリーは `(trusted_connection, tool, evaluated_definition)`、固定の日本語操作タイトル、許容schema、引数別役割、privacy規則、作用規則、表示順序、適格な操作集合、肯定／否定fixture IDを持つ。342件台帳のハッシュは観測した版の証拠であり、未レビューのハッシュを自動的に信頼登録しない。

型だけで意味を決めない。通常のscalar／arrayでも、target（対象）、content（入力・変更内容）、option（条件）、protocol（cursor等）、private（個人・秘密）、program（実行内容）をツール別に決める。公開本文は固定のラベルと型付きの値から生成し、キー名をそのまま人間向け説明の代わりにしない。未知キーはunknown_arguments。

- 文字列は原文の意味・全文を保存し、制御文字・双方向制御・曖昧なescapeは直接承認不可とする。改行・タブを含む通常本文は安全な表示上の表現へ変換し、原文との違いを明示する。Markdown・メンション・リンクを命令や通知として実行しない。
- boolean、数値、null、未指定を区別する。nullと省略で意味が変わるpatch APIは「維持」「消去」「値の設定」を混同しない。
- arrayは件数と全要素を順序付きで示す。省略して許可ボタンを残さない。
- 構造化条件は評価済み文法の式として示す。AND/OR、否定、範囲、並び順を省略しない。オブジェクト全体をJSON dumpして「実操作を説明済み」と扱わない。
- 既定値は確認済みのツール契約からだけ補完する。現在ページ・対象文書の内容や権限をモデル説明から推定しない。

## 4. 構造化引数の詳細評価対象

台帳の最新分類は構造化規則30引数・構造化検討4引数、本文等42引数・本文等の検討20引数。以下を投影仕様へ落とし込む。これはトップレベル引数の分類件数であり、意味評価・fixtureの完成件数ではない。単なる未実装をprivacy理由へ変換しない。

| 引数群 | 必要な解釈・完全表示 | 追加の個別確認条件 |
| --- | --- | --- |
| document_control.items | surface、具体tool、versionの全組。照会する定義を表示 | execute_document_command.argsは別の内包操作schema取得・照合が必要 |
| github.pull_requests | repositoryとPR番号の対応を全件表示 | 省略・不正selectorは承認不可 |
| github.file_comments / tree_elements | 各ファイル、位置、本文／blob／modeと変更種別 | リポジトリ更新は外部送信、削除要素は削除。毎回確認 |
| Gmail MIME / labels | MIME tree、添付、本文、ラベル／fieldsを完全に本人限定で表示 | メール全体は公開しない。送信・転送・削除は単発 |
| Calendar reminders / attendee_optionality | 既定通知使用と個別通知、宛先と任意参加を区別 | 参加者は個人情報。通知や出欠送信は外部送信 |
| Drive write_control | revision前提とtarget revisionを区別 | requestsの任意操作を単なる更新と縮約しない |
| Drive batchUpdate requests | 文書・Sheet・Slidesごとの操作tag、対象範囲、変更内容を専用文法へ投影 | 未評価tagはoperation_semantics_unverified。変更・削除の実作用が不明ならターン許可不可 |
| Drive comments/replies/resolutions | 全内容・reply先・解決対象を対応付ける | 外部送信として単発、連絡先等は本人限定 |
| Notion parent / new_parent | page／database／data_source／workspaceの択一を明示 | 親変更による可視性への影響が確定しない場合は共有変更相当の単発 |
| Notion filters / sorts / query-data-sources | property、演算子、値、AND/OR、順序、rows/view/sqlを区別 | SQL／configure／DDLはその文法の実作用検証を必要とする。任意コードと安全な読取式を名前で同一視しない |
| Notion rich_text / pages / properties / content_updates | text、リンク、mention、位置、置換全件、添付を区別 | 個人mentionは本人限定、外部公開・削除は単発 |
| plugin_management.updates | globalとapp override、inherit、変更後の権限範囲を全て表示 | 共有／実行権限変更は単発。通常の読取と分離 |
| safety_settings.value | boolean、時間帯、一覧のunionを区別し、上流の確認要約と照合 | 全体本人限定。上流が指定する明示本人承認を自動化しない |
| Sites tunnel_bindings / access changes | bindingと接続先、追加／削除viewer/editorの対応を示す | 公開deploy・権限変更は単発。個人の識別情報は本人限定 |
| browser_fill_form.fields | 全欄のtarget、name、type、value、送信可能性を示す | password等は本人限定。入力イベントの外部送信を否定できない場合は単発 |
| browser_drop.data | MIME、対象、内容とファイルを区別 | 外部送信なので単発。秘密／個人情報は本人限定 |
| browser_webmcp_call.params | frame、内包tool、内包schemaと実作用の照合が必要 | 現在の外側定義だけでは意味を保証できない。単発の詳細確認を維持 |
| hf_jobs.args | operationごとの読取／実行／取消／費用等を区別 | 実行コードと資格は本人限定かつ単発。listを根拠にrunを自動許可しない |
| Serena occurrence_ids | 返却されたoccurrenceの型と対応を検証 | schemaのitems型不足を導入ソースで確認。第6節のstring配列規則を適用し、実接続版と受入時に再照合する |
| 通常のcomment/content/repl/body等 | 非機密の原文全文と変更対象を示す | 本人限定条件を検査。書き込むsource codeと直ちに実行するcodeは作用を分ける |

この表は詳細設計の作業対象と解釈規則であり、未確認の内包schemaやプライバシー判定を確認済みと偽るものではない。内包操作の不足を明示し、アダプタごとの具体ラベル・条件式・fixtureを完成させることを実装開始条件に含める。

## 5. 複合ツールと依頼中許可

grant_scopeはturn_toolを維持するが、0.5では「当該policy世代で評価した通常操作」に限定する。全引数を無条件に許可する意味ではない。scopeへpolicy_generationとdefinition_generationを追加し、既存scope境界も全て維持する。Gatewayは最初に許可の対象操作と毎回確認する例外を表示する。

例：browser_tabsはlist/new/selectを通常操作集合、closeを削除の個別確認とする。初回listの許可はcloseへ適用しない。list限定の引数一致ルールをユーザーに設定させることもしない。browser_findはtext/regexとも毎回内容を検査し、個人情報・秘密の入力に変わったら公開・自動適用しない。

すべての呼出しで単発確認ルールを先に評価する。allowlistや古いgrantが上位にあっても外部送信・共有変更・削除・実行コード・秘密・認証を迂回させない。新しい未評価引数・schema変更・call不一致・catalog失効時も自動適用不可。

公開しない個人情報を含む読取と秘密入力を区別する。本人限定で明示作成したread grantの適用条件も同じprivacy方針へ拘束し、後続の公開カードへ引数を漏らさない。いずれも本人限定画面を開いただけでは許可を作らない。

## 6. 確定した構造化表示規則

以下は台帳のR引数に対応する固定規則。未知のキーを読み飛ばさない。各行の値を全て表示し、長い場合は本人向けの全体確認へ移す。実装済みを意味しない。

| 規則ID | 受入形・表示順・作用 |
| --- | --- |
| schema_query_items | 配列の各要素surface/tool_name/versionを「文書の種類／照会する操作／版」の順で表示。照会はread。要素を省略しない |
| repository_pr_pairs | 各要素repository_full_name/pr_numberを「リポジトリ／PR番号」として対応付けて表示。read |
| calendar_reminders | use_defaultを「標準の通知を使用」、overridesの各method/minutesを「通知方法／何分前」として表示。use_defaultとoverridesの併記はschemaだけでなく上流の意味を検証。通知先等の個人情報は本人向け |
| attendee_options | email/optionalを「参加者／任意参加」で全件表示。呼出し全体を本人向け。招待等はexternal_send |
| revision_precondition | requiredRevisionIdは「一致が必要な版」、targetRevisionIdは「変更の基準にする版」。null／省略は制約なしとして区別。両方ある形を上流に照合せず勝手に受理しない |
| tunnel_binding_pairs | binding_alias/tunnel_idの全組を「接続名／トンネルID」として表示。deployの内容の一部であり、独立したread扱いにしない |
| access_changes | add/removeとeditor/viewer、account/groupをそれぞれ「追加／削除」「編集者／閲覧者」「利用者／グループ」として全件表示。本人向け、permission_change、単発のみ |
| permission_changes | global_permissions/app_permissionsを「全体の設定／このアプリの設定」に分ける。always_ask＝毎回確認、ask_before_writes＝書込み前に確認、review_important_actions＝重要な操作を確認、full_access＝全アクセス、inherit＝全体設定に従う。変更後の値を省略せず、単発確認。未知enumは受理しない |
| parental_value | booleanは「有効／無効」、時間objectはenabled/start_time/end_time、一覧は全要素を表示。本人向け。prepareと実適用を区別し、実適用は必ず単発 |
| browser_form_fields | 各要素target/name/type/valueを「入力先の指定／入力欄／入力の種類／入力値」に対応させる。elementは説明として併記できるがtargetの代わりにしない。秘密・個人情報を含む呼出しは本人向け。送信作用を判定できなければ単発 |
| notion_parent | page_id/database_id/data_source_id/folder_id/workspaceの択一を「移動先または作成先の種類／識別子」で示す。未知キーを許可しない。既存contentの可視性変更が否定できなければ単発 |
| notion_position | type=start/endを「先頭／末尾」と表示。既定値と省略を混同しない |
| notion_sorts | property=created_at/updated_at、direction=ascending/descendingを「作成日時／更新日時」「昇順／降順」で順序付き表示。read |
| notion_search_filters | created/edited date rangeの上下限、created_by/edited_by user IDs、teamspace IDs、title_only、content_statusを全て表示。個人のIDがある場合は呼出し全体を本人向け。日付境界の未指定を全期間と明記 |
| serena_occurrence_ids | null／省略は全候補、非空string配列は指定した候補に限定。候補IDを全件表示。空配列は不正。根拠はSerena導入キャッシュ内file_tools.pyのReplaceInFilesTool.apply引数`list[str] | None`と空配列拒否。dry_run=trueはread、falseはwrite |

追加の規則（隔離取得した実定義の引数説明・型と照合）：

| 規則ID | 受入形・表示順・作用 |
| --- | --- |
| github_review_comments | 各項目のpath/bodyを「対象ファイル／コメント全文」として必須表示。positionは「差分内の位置」であり行番号へ変換しない。line/sideとstart_line/start_sideは「終了行／終了側／開始行／開始側」として表示し、LEFT＝変更前、RIGHT＝変更後。未指定とnullを勝手に補完しない。未知side、位置指定が矛盾する場合は承認不可。レビュー全体のaction・本文・commitも同じ表示へ含め、external_send、単発 |
| drive_comment_replies | 各要素comment_id/contentを「返信先コメントID／返信全文」として対応付けて表示。配列順序を保持。元ツールのfile_idと合わせ、別ファイルのスレッドへ読み替えない。external_send、単発 |
| drive_comment_resolutions | comment_id/reply_contentを「解決済みにするコメントID／解決時の返信全文」として表示。返信の省略は「返信を追加しない」。空文字とnullはそれぞれ表示し、同じ値へ書換えない。解決操作を単なるコメント読取と扱わず、external_send、単発 |
| gmail_classification_labels | 各label_idとfields内のfield_id/selectionを「組織の分類ラベルID／分類項目ID／選択値ID」として全件対応付けて表示。INBOX等の受信箱ラベルと混同しない。名称は取得していないためIDから推測しない。update_draftでは省略・null＝既存を維持、空配列＝既存分類を全解除、非空＝指定分類へ置換。呼出し全体は本人向け。send_emailはexternal_sendで単発。create/update_draftの作用はツール全体で別に判定する |

これらの規則を台帳へ対応付けたことは、当該ツールの全引数設計・fixture・実行受入の完了を意味しない。Drive bulkのcommentsにあるanchor、Gmail MIME本文等は引き続き別の詳細評価対象。省略可能な項目の未指定も説明に含める。

Serenaのoccurrence_idsはMCP schemaのitems型が欠けていたが、導入キャッシュ内の2版（a5fd4d68/18fa47bf）の型定義と処理を確認した。ソース文字列は個人のファイルパスを製品文書へ固定せず、受入時に実接続の版と再照合する。静的資料確認と実呼出し試験を区別する。

## 7. プライバシー判定の最低限の検証集合

PI01: Gmail全体、Notionの個人宛てsession、家族設定を、引数のキーにpasswordがなくても本人向けにする。
PI02: to/cc/bcc/from_address/reply_to/user_email/email/username/user_id/attendees等の具体的な個人情報を持つフィールドを本人向けにする。許可の読取／書込み判定とは分ける。
PI03: メールアドレス、国際／国内電話、郵便番号と詳細住所の組、個人番号・資格情報の識別名と値、既知の認証ヘッダー・署名URLを本人向けにする。部分伏字で公開しない。
PI04: 登録済みのqueryにもPI03を適用する。入力値をURL query、percent encoding、改行、JSON文字列へ埋め込んでも検査を抜けない。
PI05: 通常の非機密文（検索語、数値、製品名、一般の公開URL、通常の編集文）を過剰に本人向けへ落とさない。公開先は元会話に限定。
PI06: 独自に偽装された個人情報・自然文中の実名を完全に自動識別すると保証しない。入力の意味が曖昧な構造や未評価フィールドはprivacy_unclassified。これを全自由文に機械適用して通常表示を放棄しない。

正規化・判定文法・肯定／否定の具体値は[公開範囲の判定仕様](mcp-approval-privacy-design.ja.md)で定義する。これらは最低限の試験集合であり、ツールごとの具体フィールドへの対応を実装開始前に完成させ、承認時の再評価と同一ロジックを使う。プライバシーの判定結果をモデル生成の説明から推定しない。

## 8. 台帳の作用候補とAPIの区別

台帳のeffectsは設計用の候補であり、その文字列をAPIへそのまま返さない。`dynamic`は内包操作の評価結果、`file_write_optional`はファイル保存引数の有無、`delete_tab`はclose分岐、`network_navigation`はアクセス先・入力の評価結果、`external_send_unverified`は送信作用の未確定を表す。各候補はツール別条件式でAPIのread/write/delete/external_send/permission_change/execute/credential/session_change/unknownへ変換する。未確定をreadへ丸めない。unknownを含む呼出しに依頼中許可を作らない。

作用と公開範囲を独立して確定できる場合は、本人向け表示でも通常読取の許可範囲を狭める理由にしない。一方、操作内容そのものを確定できない場合は単なるプライバシー例外と表示しない。既存の個別確認へ戻せる条件と、完全な説明を作れず許可不可となる条件は、内包操作の詳細設計で明記する。未評価行を一律にこの例外へ逃がして342件対応完了とはしない。

## 9. Notion複合操作の作用確認

[Notion詳細設計](mcp-approval-notion-design.ja.md)へ提供元のDSL／Markdown仕様の照合結果を追加した。viewのFORM設定は権限変更、本文の既存page/database指定は移動、allow_deleting_contentは子リソース削除の条件である。通常のwrite候補へ一括分類しない。台帳のoperation_casesと20件の設計期待値へ反映。完全な文法・属性型の取得／照合・全表示ラベルの完成までは、P/Sや詳細設計未完了の印を消さない。

部分置換はnew_str単体の分類で完結させない。対象本文と全置換を提供元の意味に従って評価し、必要なら中間作用も判定する。対象版への対応付け・更新競合防止の根拠を承認へ拘束する。詳細はNotion設計第3.1〜3.2節。読取根拠が不足した場合を、本人向け単発確認だけで回避しない。

現版のNotion部分置換は利用者決定により実行不可（[Notion第3.2節](mcp-approval-notion-design.ja.md)）。上記の対象本文・順序・版固定は将来許可対象へ広げる場合の条件であり、現在のupdate_contentを単発許可へ落として実行する条件ではない。

## 10. 追加の複合入力の具体化

- github_tree_elements：[GitHubツリー設計](mcp-approval-github-tree-design.ja.md)、GT01〜17。mode/type、sha/content排他、親子パス、削除、秘密と表示を規定。
- drive_comment_creates：[Driveコメント設計](mcp-approval-drive-comments-design.ja.md)、DC01〜12。既存返信／解決規則と合わせ、1〜20件、対象の同一性、引用・位置指定・anchorを規定。

台帳へ規則とfixtureを対応付けた。従来のP/S集計は全体レビュー前の棚卸し区分として残す。新規の2引数は具体規則追加済みだが、他の検討引数まで完了にしない。

責務整理合意後の接続先は[API 0.6レビュー案](mcp-approval-api-v06.ja.md)。本書の技術的知見は引き継ぐが、単発承認への意味評価必須条件・全実行への禁止波及・全342件の着手ゲートは後継要件に従って置き換える。
