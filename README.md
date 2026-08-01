# memory_server

エージェントが記録する Thought を SQLite に保存し、localhost の HTTP MCP として公開する Rust サーバーです。Thought は過去を上書きせず、現在重視している Thought は `active_set` で管理します。

```text
エージェント ── HTTP MCP ── memory_server ── SQLite
                                ↑
memory_viewer ── 読み取り API ┘
```

サーバーは localhost 専用です。標準入出力の MCP、外部公開、認証、TLS は提供しません。

## 起動

Rust の安定版を用意し、リポジトリ直下で実行します。

```bash
MEMORY_DB_PATH=./memory.sqlite3 cargo run
```

既定の接続先は次のとおりです。

- MCP: `http://127.0.0.1:3000/mcp`
- Viewer 向け読み取り API: `http://127.0.0.1:3000/api/*`

`MEMORY_DB_PATH` を省略すると、カレントディレクトリの `memory.sqlite3` を使います。DB は初回起動時に作成され、起動時に必要なスキーマ移行を行います。

## 設定

設定ファイルは使わず、必要な項目だけ環境変数で指定します。

| 環境変数 | 既定値 | 意味 |
| --- | --- | --- |
| `MEMORY_DB_PATH` | `memory.sqlite3` | SQLite データベースのパス |
| `MEMORY_HTTP_PORT` | `3000` | HTTP の待受ポート |

サーバーの bind 先は常に `127.0.0.1` です。

```bash
MEMORY_DB_PATH=./data/agent.sqlite3 MEMORY_HTTP_PORT=3001 cargo run
```

## HTTP エンドポイント

| メソッド | パス | 用途 |
| --- | --- | --- |
| `POST` | `/mcp` | JSON-RPC による MCP 操作 |
| `GET` | `/api/memories` | memory_id の一覧 |
| `GET` | `/api/memories/{memory_id}/context` | active_set と現在文脈 |
| `GET` | `/api/memories/{memory_id}/thoughts` | Thought 全履歴 |

`/api/*` は別プロジェクトの Viewer がサーバー側から読む内部 API です。ブラウザから記憶サーバーへ直接アクセスする用途ではありません。MCP 操作と記憶モデルの詳細は [REQUIREMENTS.md](./REQUIREMENTS.md) を参照してください。

## 関連プロジェクト

- [`memory_viewer/`](./memory_viewer/README.md) — 記憶を読み取り専用で表示する独立 Rust プロジェクト
- [`experiments/`](./experiments/README.md) — Blicket 環境と LLM 比較を置く独立 Rust プロジェクト。本体サーバーの依存には含まれません

## 開発

```bash
# memory_server
cargo test

# Viewer
cargo test --manifest-path memory_viewer/Cargo.toml

# 実験
cargo test --manifest-path experiments/Cargo.toml
```

GitHub では [CI ワークフロー](./.github/workflows/ci.yml) が各 push と pull request で、整形と三プロジェクトのテストを実行します。

設計上の決定と今後の接続作業は [MINECRAFT_MEMORY_ROADMAP.md](./MINECRAFT_MEMORY_ROADMAP.md)、保留事項は [TODO.md](./TODO.md) に記録しています。
