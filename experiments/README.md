# memory_experiments

`memory_experiments` は、記憶サーバー本体から切り離された Blicket 検証・LLM 実験プロジェクトです。`mineplan` の Thought ストアをライブラリとして利用しますが、HTTP MCP サーバーには含まれません。通常の mineplan 運用では起動不要です。

以下はリポジトリ直下から実行します。

```bash
# 決定的な参照ランナー
cargo run --manifest-path experiments/Cargo.toml --bin blicket_memory_demo

# LLM 比較
OPENAI_API_KEY=... cargo run --manifest-path experiments/Cargo.toml --bin blicket_llm_experiment
```

LLM 比較は `OPENAI_MODEL`、`BLICKET_MODE`、`BLICKET_RESULT_PATH` でも調整できます。結果は既定で `blicket_llm_result.json` に保存されます。

テストは次で実行できます。

```bash
cargo test --manifest-path experiments/Cargo.toml
```
