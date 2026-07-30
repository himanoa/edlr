---
paths:
  - "core/**/*.rs"
---

# 手続きを綺麗にする作法

原則は一点: **判断と実行を分ける**。手段は普通の関数抽出で、
新しい機構(Effect enum・interpreter)は導入しない。

## 判断は純関数に抽出する

手続き中の `if`/`match` の塊は、名前のついた純関数(値イン値アウト)に切り出す。
抽出した関数がそのまま純粋テストの対象になる。

```rust
// ❌ 実行の中に判断が埋まっている
fn restart(&mut self, id: &str) -> Result<(), Error> {
    if let Some(inst) = self.instances.get(id) {
        if inst.state == State::Running && !inst.stopping {
            self.stop_process(id)?;
            // ...
        }
    }
    // ...
}

// ✅ 判断を純関数へ。実行側は結果に従うだけ
fn should_restart(inst: Option<&Instance>) -> RestartAction { /* 純関数 */ }

fn restart(&mut self, id: &str) -> Result<(), Error> {
    match should_restart(self.instances.get(id)) {
        RestartAction::StopThenStart => { self.stop_process(id)?; /* ... */ }
        RestartAction::StartOnly => { /* ... */ }
        RestartAction::Noop => Ok(()),
    }
}
```

## 命令的関数は短く・直線的に

- 目安1関数1画面(〜40行)、ネスト2段まで
- 深くなったら early return・ガード節・関数抽出で平らにする

## 読み → 判断 → 書き の順に整える

読み書きが交互に出てくる手続きは、先に読む・真ん中で判断・最後に書く順に
並べ替える。ロック取得も「読みの前・書きの前」に自然と整列する。

## 判断結果が複数あるときは小さな構造体で返す

`runner::LoopAction` が既にやっている形。**そうすると綺麗になる場所でだけ**使い、
全操作に義務付けない。
