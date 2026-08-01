# To Do

Minecraft の AI エージェント用記憶基盤として、今後検討・実装する項目を保存する。

## 確定事項

- 永続ストアは SQLite を使う
- 長期記憶の所有単位は `memory_id` とする
- epoch は採用しない。`memory_clear` は memory_id を残して内容だけを即時に削除する
- 長期記憶の記録単位は Thought のみとする。Observation、Event、行動専用レコードは持たない
- エージェントが `active_set` を操作し、その中のアンカーを管理する
- `memory_get_context` は active_set の現行アンカー周辺を返す
- Thought に `next_action` は持たせない。行動実行は外部ツールに委ねる
- active_set のアンカーは Thought ID のみとする
- コンテキスト取得はアンカーから `associated_from` と `related` を双方向 BFS し、発見順で最初の指定件数を返す。既定上限は 50 件で、呼び出し側が変更できる
- `related` は同一 memory_id 内の二つの Thought を結ぶ、名前なし・双方向の可変リンク。自己関連・重複は許可しない
- BFS の辺は新しく追加されたものを優先する
- 長期検索・タグ付け・埋め込み検索は初版に含めない
- 観測事実・行動・解釈は、区別せずエージェントが Thought の premise として記録する

## MCP 記憶サーバー

- SQLite 永続ストア、Thought の追記、`memory_get_context`、active_set、related、memory_clear を localhost の HTTP MCP として実装済み
- SQLite スキーマは `schema_migrations` で版管理し、移行管理のなかった旧 DB も現行形式へ移行する
- Rust 側が `T1`, `T2`, ... と premise ID を `T1.P1`, ... の形で採番する
- 振り返りも特別な API ではなく Thought として追記する

## active_set とコンテキスト

- active_set は memory_id ごとに一つ、Thought ID の順序付きリストとして保存する
- 追加・削除・置換（並べ替え）を MCP 操作として実装済み
- Thought 間の `associated_from` を双方向に BFS し、深さではなく発見順の最大件数で打ち切る
- active_set が空なら通常の文脈取得は空を返す
- memory_clear は空の記憶に対しても成功し、削除件数 0 を返す

## Thought 専用モデルへの移行

- Event / Observation / `next_action` 依存を削除し、Thought 専用ストアへ移行済み
- 事実・行動・解釈を premise の自由文として記録する
- Minecraft の位置、時刻、所持品、近傍、体力、行動結果は、必要なものだけ premise に記録する
- 実際の Minecraft 接続方式が決まった時点で、エージェントが Thought を記録する運用を接続する

## 記憶の取得・要約

- 全履歴を毎回渡さず、active_set の Thought アンカーとその周辺を優先して取得する
- 要約を導入する場合も、元の記憶項目への参照を残す
- 将来、要約を導入する場合は元 Thought への参照を残す

## 次に検討すること

- 実際の Minecraft 接続方式と、どの局面でエージェントが Thought を追記するか
- active_set の更新を促すエージェント側の運用規約
- タグ・検索・要約が必要になった時点での追加設計
- `experiments/` の Blicket 記憶検証環境を使った、MCP 接続済み LLM エージェントの実行・比較
