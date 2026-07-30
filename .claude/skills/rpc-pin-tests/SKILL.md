---
name: rpc-pin-tests
description: core リファクタリング Phase 2(rpc + server)に着手する前に必ず使う。代表的な RPC 応答の生 JSON を捕捉する pin テストを追加する手順。server.rs の巨大 match を分解する前の防衛線。
---

# rpc-pin-tests

Phase 2 で `server.rs` の巨大 match を `rpc/` の小関数群へ分解する前に、
代表的な RPC 応答の**生 JSON 全体**を等値比較で固定するテストを追加する。
分解後に JSON の形が 1 フィールドでも変われば落ちる、というのが目的。

pin テストの**追加**はテスト凍結原則と矛盾しない(既存テストに触らないため)。
リファクタリング終了後も残す。

## 手順

### 1. 対象メソッドを列挙する

ディスパッチは `core/src/server.rs` の `match method` (2箇所)にある:

```bash
grep -n 'match method' core/src/server.rs
grep -oE '"[a-z_]+(/[a-z_]+)?" =>' core/src/server.rs
```

代表を選ぶ基準: `*_result_json` 群を最も多く通るメソッドを優先する。
最低限入れるもの:

- `plugins/list`(capabilities / sidecars / filesystem / bus / dashboard /
  schedules / dropped / secretsSet / state をすべて通る最重量応答)
- drivers 側の `list`(plugin 側と同型コードの응答。Phase 4 のジェネリック共通化でも錨になる)
- grant 状態・schedule を含む応答が上記で薄い場合は、それらを含むメソッドを追加

### 2. テストファイルを作る

`core/tests/rpc_pin_integration.rs` を新規作成する。既存の
`core/tests/ws_rpc_integration.rs` と `core/tests/support/` のハーネス
(デーモン起動・WS 接続・fixture プラグイン配置)をそのまま流用すること。
新しいハーネスを発明しない。

### 3. 生 JSON を捕捉して固定する

1. まずテストを「応答を eprintln する」形で書いて1回実行し、実際の JSON を得る
2. 得られた JSON を **そのまま** `serde_json::json!` リテラルとしてテストに貼る
3. アサーションは全体の等値比較にする:

```rust
let expected = serde_json::json!({
    "pluginsDir": plugins_dir_str,
    "plugins": [ /* 捕捉した生 JSON をそのまま */ ],
});
assert_eq!(response["result"], expected);
```

- 実行ごとに変わる値(ディレクトリパス・ポート・タイムスタンプ)だけは
  変数化 or 事前に該当フィールドを比較対象へ差し込む。
  **それ以外のフィールドを削ったり部分比較にしたりしない**(部分比較にすると
  形の変化を検出できず pin の意味がなくなる)

### 4. 分解作業中の扱い

- Phase 2 の各コミットで `cargo test --workspace` に含めて回す
- pin テストが落ちたら応答の形が変わっている。**テストを直すのではなく実装を戻す**
- 意図的に応答形式を変えたくなったら、それは挙動不変リファクタの範囲外。
  別タスクとして git issues に起票する
