# mineplan設計変遷

取得元: `https://github.com/sugipamo/mineplan`

取得した `main` のリビジョン: `5164b2dfc94550ec4fe09ef088a9114ff45f6a29`

この環境にはgitがなかったため、GitHubの `main` ソースアーカイブを展開しました。そのため作業ツリーに `.git` は存在せず、元リポジトリへ変更が送られることもありません。

## 引き継いだ特性

- Rust、Axum、rusqliteによる小さなlocalhostサーバー
- HTTP上のMCP JSON-RPC
- SQLiteによる永続状態
- ID単位で独立したグラフ
- 登録順を使った決定的な探索結果
- MCPと読み取りAPIが同じストアを共有する構成
- インメモリSQLiteを使う単体・HTTPテスト
- 外部Originを拒否するlocalhost前提
- 起動時に段階的なスキーマ移行を行う構成

## 置き換えた特性

| 旧mineplan | 現mineplan |
| --- | --- |
| Thoughtとpremise | 本文自体を識別子とする自由文note |
| 無向の文脈探索 | 永続 `edge_id` と `edge_name` を持つ有向の前後関係 |
| active_setを起点にBFS | 呼び出し時の複数focusから最大limit件を双方向探索 |
| 近傍のThought一覧 | edge_nameごとのprevious / next表示とローカルSCC |
| Thought merge | String ID変更と既存ノードへの統合 |

## 引き継がなかった特性

- Thoughtの追記専用履歴
- active_set
- 名前なしの無向関連
- 双方向BFS
- 全記憶を対象にしたSCC分析

旧 `src/thought.rs`、`memory_viewer`、`experiments` は、現在のAPIと両立しないため削除しました。必要な由来情報はこの文書とライセンスに残しています。

現mineplanはタスク管理を行いません。前後欄に表示されたノードが完了済み、予定済み、または真であるとは解釈しません。辺は同じ端点・同じedge_nameでも重複登録でき、edge_idで個別に更新・削除できます。

記憶の物理削除は通常のMCP操作から外し、対象 `memory_id` の一致確認を要求するCLI管理操作に限定しています。
