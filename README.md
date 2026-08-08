# mineplan

自由文のメモと理由付きの前後関係をSQLiteへ保存し、指定したメモを中心として前提・過去と後続・未来を取り出す外部記憶MCPです。

前後関係は、内容の真偽、タスクの完了、将来の予定を保証しません。ツールが保持するのは、呼び出し側が宣言した「このメモは、あのメモより前」という順序だけです。

```text
前提・過去 → フォーカス → 後続・未来
```

## モデル

```text
木を集める → 板材を作る → 棒を作る → つるはしを作る → 採掘に行く
                              ↑ focus
```

`棒を作る` をフォーカスにすると、次のように返ります。

```json
{
  "before": [
    ["木を集める"],
    ["板材を作る"]
  ],
  "focus": [
    ["棒を作る"]
  ],
  "after": [
    ["つるはしを作る"],
    ["採掘に行く"]
  ],
  "connections": [
    {
      "before": "板材を作る",
      "after": "棒を作る",
      "reason": "棒の材料として板材を使うため"
    }
  ]
}
```

`before`、`focus`、`after` はすべて二重配列です。内側の配列は、取得した最大limit件の部分グラフ内で同じ強連結成分（SCC）に属するメモ群です。前提側は遠いメモ群からフォーカスへ、後続側はフォーカスから遠いメモ群へ並びます。関係のないメモは含まれません。`connections` には表示範囲内の辺と理由が入ります。

## MCPツール

| ツール | 用途 |
| --- | --- |
| `note_add` | 単独の自由文メモを追加 |
| `note_rename` | メモのString IDを変更。変更先が存在すればノード統合 |
| `order_add` | 理由付きの `before → after` を追加。未知のメモは自動作成 |
| `memory_focus` | limit件のローカルグラフをSCC分析して取得 |

LLM向けMCPはこの4ツールだけを公開します。利用する記憶はサーバー起動時の `MEMORY_ID` で固定されるため、各ツールへ `memory_id` を渡す必要はありません。管理用HTTP APIと全件取得ツールは公開しません。

### 前後関係を記録する

```json
{
  "name": "order_add",
  "arguments": {
    "before": "板材を作る",
    "after": "棒を作る",
    "reason": "棒の材料として板材を使うため"
  }
}
```

辺は `before + after + reason` の3点で識別します。3点が完全に同じ宣言を繰り返した場合は `added: false` を返します。同じ前後でも理由が異なれば、別の辺として追記されます。

逆方向の辺も独立して登録できます。例えば `A → B` と `B → A` は、それぞれ固有のreasonを持つ2本の有向辺として保持されます。双方向辺、3ノード以上の循環、自己辺はいずれも正常な記憶です。`memory_focus` の取得範囲に循環全体が含まれる場合、それらのノードは同じSCCとして一つの内側配列にまとめられます。

### メモのString IDを変更する

```json
{
  "name": "note_rename",
  "arguments": {
    "from": "木材を集める",
    "to": "原木を集める"
  }
}
```

`to` が未登録なら単純な改名です。既に存在する場合は両ノードを統合し、すべての辺を付け替えます。付け替え後に `before + after + reason` が完全一致する辺は一つに統合されます。循環や自己辺になった関係もreasonを失わず残ります。変更元ノードは物理削除されるため、この操作は実質的な削除を含みます。

### フォーカスを取得する

```json
{
  "name": "memory_focus",
  "arguments": {
    "focus": ["棒を作る"],
    "limit": 50
  }
}
```

`focus` は複数指定できます。前方向と後方向を近い順に探索し、明示フォーカスを含むユニークなメモを最大 `limit` 件取得します。既定値は50です。

- SCC分析は全記憶ではなく、取得した部分グラフ内だけで行います。
- limit途中で循環が切れた場合、見えている範囲だけでbefore／focus／afterを分類します。
- limitを増やすと、遠くの戻り辺が見えてSCC分類が変わることがあります。
- `connections` も独立して最大limit件です。
- 明示フォーカスの件数自体がlimitを超える場合はエラーです。

MCP応答はLLMがそのまま読む前提で、JSONを `content` に一度だけ返します。書き込み結果は `added`、または `changed` と `merged` のみに絞り、`memory_focus` は `before`、`focus`、`after`、`connections` のみを返します。limitによる省略は通常動作として扱い、完全取得かどうかを示すフラグは返しません。

## DBマイグレーション

起動時に専用の `ordered_memory_schema_migrations` テーブルを確認し、必要な移行をトランザクション内で実行します。

理由が存在しなかった旧スキーマの辺は保持され、次の理由が設定されます。

```text
未登録
```

この移行は繰り返し起動しても再適用されません。プログラムより新しいDBスキーマを検出した場合は、破壊的な起動を避けるためエラーになります。

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
