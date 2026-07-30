---
paths:
  - "core/**/*.rs"
---

# 純粋 / 命令的モジュールの境界

## 禁止

純粋モジュール(`manifest` `capability` `settings` `schedule` `rpc` `journal`)から:

- 命令的モジュール(`registry` `runner` `host` `server`)の import
- `std::fs` / `std::net` / `std::thread` / `std::process` の使用
- `Mutex` / チャネル / スレッド生成

これらが見えたらレビューで弾く。

```rust
// ❌ 純粋モジュール(rpc/)から命令的モジュールを import
use crate::registry::Registry;

pub fn render_status(registry: &Registry) -> serde_json::Value { /* ... */ }

// ✅ 値を受け取り値を返す。registry から値を取り出すのは呼び出し側(server/)の仕事
pub fn render_status(plugins: &[PluginStatus]) -> serde_json::Value { /* ... */ }
```

## 依存方向

- 純粋 → 純粋のみ許可(例: `manifest → capability`)
- 命令的 → 純粋は自由
- 純粋 → 命令的は禁止

## 副作用の置き場

ディスク永続化・Mutex・プロセス起動停止・スレッド・チャネル・wasm 呼び出し・
ネットワークは命令的モジュールへ集める。時間がかかる・失敗しうる・順序が
意味を持つ操作はすべてここ。

## 違反を見つけたら

純粋モジュール内に副作用が必要になったら、実装を止めて境界を見直す:

1. 判断部分を純関数(値イン値アウト)に抽出する
2. 副作用は trait(`capability::GrantStorage` など)越しにするか、命令的モジュール側に移す
