# memory_viewer

`memory_viewer` は、Thought 記憶サーバーの読み取り API を表示する独立した Rust プロジェクトです。記憶サーバーや SQLite へ直接は接続せず、localhost の HTTP API だけを読み取ります。

## 起動

まずリポジトリ直下で記憶サーバーを起動します。

```bash
cargo run
```

別のターミナルで、同じリポジトリ直下から Viewer を起動します。

```bash
cargo run --manifest-path memory_viewer/Cargo.toml
```

Viewer は既定で `http://127.0.0.1:3100`、記憶サーバーは既定で `http://127.0.0.1:3000` を使います。`MEMORY_VIEWER_PORT` と `MEMORY_VIEWER_BACKEND` で変更できますが、backend は localhost の `http` URL だけを許可します。

```bash
MEMORY_VIEWER_PORT=3101 \
MEMORY_VIEWER_BACKEND=http://127.0.0.1:3001 \
  cargo run --manifest-path memory_viewer/Cargo.toml
```

ブラウザでは `http://127.0.0.1:3100/?memory_id=<memory_id>` を開けます。Viewer は読み取り専用で、Thought や active_set を変更する操作を持ちません。

## テスト

```bash
cargo test --manifest-path memory_viewer/Cargo.toml
```
