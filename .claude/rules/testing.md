---
paths:
  - "core/**/*.rs"
---

# テスト戦略(二層)

## 二層の役割分担

| 層 | 何を使う | 役割 |
|---|---|---|
| 統合テスト(既存) | 実ディスク・実スレッド | **挙動の錨**。消さない・書き換えない |
| 純粋テスト(新規) | モック or 値の等値比較 | 分解した単位の**仕様書** |

役割が違うので、純粋テストを足しても既存の統合テストは消さない。

## 新規ロジックの書き方

1. まず判断を純関数に抽出する(→ procedure-style.md)
2. その純関数に対して値イン値アウトのテストを書く。時刻が絡むなら `now` を引数で渡す
3. 永続化が絡むなら trait(`GrantStorage` / `Storage` など)のモック越しにテストする

## モックは test_support に手書き

モックは各モジュールの `#[cfg(test)] mod test_support` に手書きする。
mockall 等のマクロ crate は導入しない。

```rust
// ✅ capability/grants.rs 内
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;

    pub struct InMemoryGrantStorage {
        pub grants: HashMap<String, GrantState>,
    }

    impl GrantStorage for InMemoryGrantStorage { /* ... */ }
}
```

## モックしないもの

wasmtime の `Store` などモックしても意味のない部分は具象のまま。
無理に trait 化しない(→ trait-di.md)。
