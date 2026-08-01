# Minecraft 記憶基盤ロードマップ

## 現在地

Thought 専用の SQLite 記憶基盤と localhost の HTTP MCP サーバーを実装した。

```text
Thought（主体の可変な前提）
  ↔ associated_from（過去 Thought への連想）
  ↔ related（後から結ぶ双方向の関連）
  ↔ active_set（現在の文脈アンカー）
```

観測事実・行動・解釈は、いずれも Thought の premise として主体が記録する。前提は矛盾・重複を許可し、過去 ID の破損だけを構造エラーとする。

## 実装済み

- `memory_id` ごとの SQLite 永続化
- 不変 Thought と自由文 premise。空の premise リスト、矛盾・重複した内容は許可する
- Thought ID のみで構成する一つの順序付き active_set
- `associated_from` と `related` を双方向 BFS し、新しい辺を優先して発見順の先頭 N 件を返す可変上限の文脈取得
- Thought の作成、active_set の取得・置換・追加・削除・並べ替え、related の追加・削除を公開する HTTP MCP
- memory_id を残して内容を即時に空にする `memory_clear` と、その削除件数の返却
- Event、Observation、`next_action`、検索・タグは公開 API に含めない

## 次の接続順

1. HTTP MCP クライアントへ `http://127.0.0.1:3000/mcp` を設定し、サーバーを `MEMORY_DB_PATH` とともに起動する
2. Minecraft 接続点が決まった時点で、エージェントが Thought を記録・active_set を更新する運用を接続する
3. 実運用で文脈不足が確認されてから、タグ・検索・要約を別機能として追加する

## 独立した記憶検証

Blicket の隠れ規則環境と LLM 比較は `experiments/` の独立プロジェクトへ分離した。記憶サーバー本体は、その実験環境に依存しない。

現時点では、仮説の真偽管理、候補辞書、巨大な知識グラフ、完全な計画器は導入しない。
