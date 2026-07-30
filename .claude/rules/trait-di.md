---
paths:
  - "core/**/*.rs"
---

# trait DI

## trait の置き場

trait は**使う機能のモジュールに置く**。中央の `ports/` ディレクトリは作らない。

既存の境界は4本:

| trait | 置き場所 | 実装者 |
|---|---|---|
| `capability::GrantStorage` | capability/ | `GrantsStore` |
| `settings::Storage` | settings/ | `SettingsStore` |
| `registry::ProcessControl` | registry/ | `ProcessDriver` |
| `registry::BusPort` | registry/ | `edlr_driver_channel::Bus` |

## trait を増やすとき

**必要が実証されたときだけ**増やす。「モックしたいテストが実在する」が実証。
推測で境界を切らない。wasmtime の `Store` などモックしても意味のない部分は
具象のまま使う。

## DI の形

generics で受け、公開面は type alias でジェネリクスを隠す:

```rust
// ✅ 内部は generics
pub struct GrantService<S: GrantStorage> { storage: S, /* ... */ }

// ✅ 公開面は alias で具象を固定
pub type DiskGrantService = GrantService<GrantsStore>;
```

```rust
// ❌ dyn Trait を標準にしない(必要が実証された場所を除く)
pub struct GrantService { storage: Box<dyn GrantStorage>, }
```

## 時刻は trait にしない

純関数が `now` を引数で受ける(sans-IO 流。quinn-proto / str0m と同じ):

```rust
// ✅ now を引数で渡す。テストは任意の時刻を渡すだけ
pub fn next_fire(schedule: &Schedule, now: DateTime<Utc>) -> Option<DateTime<Utc>> { /* ... */ }

// ❌ Clock trait や内部での Utc::now() 呼び出し
pub fn next_fire(schedule: &Schedule, clock: &dyn Clock) -> Option<DateTime<Utc>> { /* ... */ }
```

## モックは手書き

mockall 等のモックマクロ crate は導入しない。モックは各モジュールの
`#[cfg(test)] mod test_support` に手書きする(→ testing.md)。
