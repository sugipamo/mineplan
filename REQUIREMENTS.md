# Minecraft Thought 記憶基盤

Minecraft の主体が、その時点で保持する前提を連想関係とともに保存する Rust 基盤である。

```text
Thought { id, associated_from, premises }
ActiveSet { memory_id, anchor_ids }
RelatedLink { thought_id_a, thought_id_b }
```

- `Thought` は主体の自由な前提であり、観測・行動・解釈も premise として記録する
- Thought は追加のみ。過去を上書きせず、新しい Thought で前提を組み替える
- `associated_from` は過去 Thought だけを参照する
- `active_set` はエージェントが操作し、現在の文脈の中心となる Thought を保持する
- `related` は、同じ memory 内の二つの Thought を結ぶ名前なし・双方向の可変リンクである
- 文脈取得はアンカーから `associated_from` と `related` を双方向 BFS し、既定50件を発見順で返す。上限は可変であり、新しく追加された辺を優先する
- SQLite に永続化し、明示的なバージョン付きマイグレーションで既存 DB を更新して、localhost の Streamable HTTP MCP サーバーとしてエージェントへ公開する
- 読み取り専用 Viewer が、HTTP API をサーバー側から取得して active_set と文脈を確認できるようにする
- Event、Observation、検索・タグ・埋め込み検索は初版に含めない

## MCP 操作

- `memory_create` — `memory_id` ごとに長期記憶を作成する
- `memory_record_thought` — Thought を追記する。空の premise リストも許可する
- `memory_get_context` — active_set のアンカーから双方向 BFS で文脈を取得する
- `memory_get_active_set` / `memory_active_set_*` — active_set の取得、置換、追加、削除、並べ替えを行う
- `memory_related_add` / `memory_related_remove` — 二つの Thought の関連を追加・削除する
- `memory_get_related` — 指定 Thought に直接つながる関連 Thought を新しいリンク順で返す
- `memory_clear` — memory_id を残したまま、Thought・`associated_from`・`related`・active_set を即時に空にし、各削除件数を返す

MCP は `MEMORY_DB_PATH`（既定 `memory.sqlite3`）の SQLite ファイルを継続利用する。
起動時に `schema_migrations` を確認し、新規 DB は必要なスキーマを順に作成する。移行管理のない旧 DB は旧形式として検出してから移行する。現在のプログラムより新しいスキーマの DB は開かず、データの破損を避ける。

HTTP サーバーは常に `127.0.0.1` にだけ bind し、`MEMORY_HTTP_PORT`（既定 `3000`）でポートだけを変更できる。`/api/*` は外部の読み取り専用 Viewer 向け内部 API とし、ブラウザからの直接利用は許可しない。記憶を変更する操作は MCP の `tools/call` のみが担う。WebUI は別 Rust プロジェクトとして実装し、サーバー側からこの API を取得する。外部公開・認証・TLS は初版の対象外とする。

Viewer の独立プロジェクトは `memory_viewer/` に置く。Viewer も localhost だけに bind し、`MEMORY_VIEWER_BACKEND` として localhost の `http` URL だけを受け付ける。

## 独立実験プロジェクト

Blicket の隠れ規則環境、決定的な参照ランナー、LLM 比較は `experiments/` の独立 Rust プロジェクトに含める。記憶サーバーの実行ファイルと依存関係には含めない。

詳細と次の接続作業は [ロードマップ](./MINECRAFT_MEMORY_ROADMAP.md) を参照する。
