# mineplan

自由文ノードと名前付きの順序辺をSQLiteへ保存し、指定したノードを起点として関係種別ごとの前後を取り出す外部記憶MCPです。

辺は、内容の真偽、タスクの完了、将来の予定を保証しません。ツールが保持するのは、呼び出し側が宣言した名前付きの関係だけです。

```text
previous ← フォーカス → next
```

## モデル

```text
木を集める → 板材を作る → 棒を作る → つるはしを作る → 採掘に行く
                              ↑ focus
```

`棒を作る` をフォーカスにすると、関係種別ごとに次のように返ります。

```json
{
  "focus": "棒を作る",
  "groups": [
    {
      "edge_name": "task",
      "previous": [["木を集める"], ["板材を作る"]],
      "next": [["つるはしを作る"], ["採掘に行く"]]
    }
  ],
  "connections": [
    {
      "edge_id": 1,
      "edge_name": "task",
      "previous": "木を集める",
      "next": "板材を作る"
    },
    {
      "edge_id": 2,
      "edge_name": "task",
      "previous": "板材を作る",
      "next": "棒を作る"
    },
    {
      "edge_id": 3,
      "edge_name": "task",
      "previous": "棒を作る",
      "next": "つるはしを作る"
    },
    {
      "edge_id": 4,
      "edge_name": "task",
      "previous": "つるはしを作る",
      "next": "採掘に行く"
    }
  ]
}
```

`focus` は単一のノード名です。`groups[].previous` と `groups[].next` は二重配列で、内側の配列は取得した部分グラフ内で同じSCCに属するノード群です。フォーカス自身はgroupsから除外されます。探索では方向とedge_nameを維持します。`connections` は通常省略され、`include_connections: true` の場合だけ `edge_id`、`edge_name`、`previous`、`next` とともに返されます。

## MCPツール

| ツール | 用途 |
| --- | --- |
| `add_node` | `node_name` を持つ自由文ノードを追加 |
| `update_node_name` | ノード名を変更。memoは変更しない |
| `update_node_memo` | ノードのmemoを更新 |
| `delete_node` | ノードと接続辺を物理削除 |
| `add_edge` | `edge_name` 付きの `previous → next` を追加。両方向に探索可能 |
| `update_edge` | `edge_id` で辺を更新 |
| `delete_edge` | `edge_id` で辺を削除 |
| `edge_to_node` | 1本の辺を、指定ノードを経由する同名の2辺へ置換 |
| `add_sequence` | 指定順の隣接ノードへ前後探索可能な辺を一括追加 |
| `focus` | limit件のローカルグラフをSCC分析して取得 |

LLM向けMCPはこの10ツールを公開します。利用する記憶はサーバー起動時の `MEMORY_ID` で固定されるため、各ツールへ `memory_id` を渡す必要はありません。管理用HTTP APIと全件取得ツールは公開しません。

### ノードにmemoを付ける

`add_node` は任意の `memo` を受け取ります。memoを変更する場合は `update_node_memo` を使います。

```json
{
  "name": "add_node",
  "arguments": {
    "node_name": "実装する",
    "memo": "RustでMCPツールとして実装する"
  }
}
```

`focus` の応答では、memoが登録されているノードだけを `memos` に含めます。

```json
{
  "focus": "実装する",
  "memos": {
    "実装する": "RustでMCPツールとして実装する"
  }
}
```

### 前後関係を記録する

### タスク列を一括登録する

```json
{
  "name": "add_sequence",
  "arguments": {
    "sequence": ["木を集める", "板材を作る", "棒を作る"],
    "edge_name": "task"
  }
}
```

`sequence` の順番どおりに、隣接する各ペアへ1本ずつ辺を追加します。3ノードなら2本です。各辺はnext方向とprevious方向の両方へ探索でき、逆向きの辺を別途保存しません。未知のノードは自動作成され、多重辺も許可されます。

```json
{
  "name": "add_edge",
  "arguments": {
    "edge_name": "task",
    "previous": "板材を作る",
    "next": "棒を作る"
  }
}
```

辺は永続的な `edge_id` で識別します。同じ `edge_name`、`previous`、`next` の宣言を繰り返しても、それぞれ別の辺として保持され、新しい `edge_id` が発行されます。

逆方向の辺も独立して登録できます。例えば `A → B` と `B → A` は2本の有向辺として保持されます。双方向辺、3ノード以上の循環、自己辺はいずれも正常な記憶です。`focus` の取得範囲に循環全体が含まれる場合、それらのノードは同じSCCとして一つの内側配列にまとめられます。

### ノード名を変更する

```json
{
  "name": "update_node_name",
  "arguments": {
    "from_node_name": "木材を集める",
    "to_node_name": "原木を集める"
  }
}
```

`to` が未登録なら単純な改名です。既に存在する場合は両ノードを統合し、すべての辺を付け替えます。完全一致する辺が生じても自動統合せず、それぞれの `edge_id` を維持します。変更元ノードは物理削除されるため、この操作は実質的な削除を含みます。

### ノードを削除する

```json
{
  "name": "delete_node",
  "arguments": {"node_name": "古い実装案"}
}
```

`delete_node` はノードと、そのノードに接続する辺を物理削除します。同じ `node_name` を再登録しても、以前の辺は復活しません。この操作は取り消せません。

### 辺を更新・削除する

```json
{
  "name": "update_edge",
  "arguments": {
    "edge_id": 14,
    "edge_name": "task"
  }
}
```

`edge_name`、`previous`、`next` のうち指定した項目だけを更新します。少なくとも1項目の指定が必要です。辺を削除する場合は次のようにします。

```json
{
  "name": "delete_edge",
  "arguments": {"edge_id": 14}
}
```

辺の削除は関係の宣言を撤回する操作であり、ノードや記憶全体は削除しません。

### 辺の途中へノードを挿入する

```json
{
  "name": "edge_to_node",
  "arguments": {
    "edge_id": 14,
    "node_name": "忘れていたタスク"
  }
}
```

対象辺が `A ─task→ B` なら、これを削除して `A ─task→ 忘れていたタスク` と `忘れていたタスク ─task→ B` の2辺を作成します。元の `edge_name` を引き継ぎ、未知のノードは自動作成します。既存ノードを指定した場合、そのmemoと既存の接続は変更しません。多重辺があっても指定した `edge_id` だけが対象です。対象辺が存在しない場合は何も変更せずエラーになり、処理全体は一括で行われます。

```json
{
  "removed_edge_id": 14,
  "added_edge_ids": [20, 21]
}
```

### フォーカスを取得する

```json
{
  "name": "focus",
  "arguments": {
    "focus": "棒を作る",
    "limit": 50,
    "include_connections": true
  }
}
```

`focus` は1つ指定します。フォーカスからpreviousとnextの両方向を探索し、到達後も同じ方向と `edge_name` を維持します。明示フォーカスを含むユニークなメモを最大 `limit` 件取得します。既定値は50です。

`memos` に存在しないノードはmemo未登録です。`"memo": ""` を指定したノードは、空文字のmemoとして `memos` に含まれます。

- SCC分析は全記憶ではなく、取得した部分グラフ内だけで行います。
- 同じノードへ複数のedge_nameで到達した場合、それぞれの系列を継続します。
- limitを増やすと、遠くの戻り辺が見えてSCC分類が変わることがあります。
- `connections` も独立して最大limit件です。
- 明示フォーカスの件数自体がlimitを超える場合はエラーです。

MCP応答はLLMがそのまま読む前提で、JSONを `content` に一度だけ返します。通常の書き込み結果は `added`、`changed` / `merged`、`updated`、`deleted` に絞り、`edge_to_node` は置換前後の辺IDを返します。`focus` は `focus`、`groups`、memoがある場合の `memos`、要求時の `connections` を返します。limitによる省略は通常動作として扱い、完全取得かどうかを示すフラグは返しません。

## DBスキーマ

新規DBは起動時に現行スキーマを作成し、現行スキーマ識別子を持つDBは次回以降も再利用します。旧版のmineplan DBは互換対象外で、自動削除・自動移行せず起動エラーになります。必要な場合はDBファイルを利用者が退避または削除して、新しいDBを作成してください。

## 起動

```bash
cargo run
```

- MCP: `http://127.0.0.1:3000/mcp`
- 既定の記憶ID: `default`
- 既定DB: `mineplan.sqlite3`

```bash
MEMORY_ID=minecraft \
MEMORY_DB_PATH=./data/memory.sqlite3 \
MEMORY_HTTP_PORT=3001 \
cargo run
```

起動時に対象の記憶が存在しなければ自動作成されます。利用可能な引数と環境変数は次のコマンドで確認できます。

```bash
cargo run -- --help
```

バイナリのバージョンは `--version` または `-V` で確認できます。

```bash
mineplan --version
# mineplan v0.5.0
```

リリースバイナリにはビルド対象のGitタグを埋め込みます。ローカルビルドでは `git describe --tags --always --dirty` の結果を表示し、Git情報が利用できない場合だけCargoのパッケージバージョンへフォールバックします。

## 記憶の物理削除

記憶の削除はMCPに公開されません。運用者だけがCLIから実行できます。`memory_id` は残り、その中のメモと辺が物理削除されます。

対話確認を使う場合：

```bash
cargo run -- clear-memory --memory-id minecraft
```

続行するには、プロンプトへ対象の `memory_id` を同じ表記で再入力します。

自動化する場合も、対象IDを確認値として明示する必要があります。

```bash
cargo run -- clear-memory \
  --memory-id minecraft \
  --confirm minecraft
```

`--memory-id` と `--confirm` が一致しない場合は何も削除しません。DBの指定にはサーバーと同じ `MEMORY_DB_PATH` を使用します。

## 開発

```bash
cargo fmt --check
cargo test
```

## 由来

旧版mineplanを履歴なしで取得し、現在の外部記憶MCPへ全面的に置き換えています。設計変遷と基準リビジョンは [DERIVATION.md](./DERIVATION.md) に記録しています。

## License

[MIT License](./LICENSE)
