---
paths:
  - "core/**/*.rs"
---

# 不必要な mut を使わない

`mut` は「ここに可変状態がある」という宣言。少ないほど読み手が追う状態が減る。
本ルールは**命令的モジュールでも同様に守る**。命令的モジュールは副作用の
置き場ではあるが、mut を自由に使ってよい場所ではない。
**値の組み立てに mut を使わない**。

## 組み立ては iterator / 式で

```rust
// ❌ mut + push で組み立て
let mut names = Vec::new();
for p in plugins {
    if p.enabled {
        names.push(p.name.clone());
    }
}

// ✅ iterator で組み立てて束縛は不変
let names: Vec<_> = plugins
    .iter()
    .filter(|p| p.enabled)
    .map(|p| p.name.clone())
    .collect();
```

```rust
// ❌ mut で条件分岐の結果を入れる
let mut timeout = DEFAULT_TIMEOUT;
if let Some(t) = config.timeout {
    timeout = t;
}

// ✅ 式として書く
let timeout = config.timeout.unwrap_or(DEFAULT_TIMEOUT);
```

## &mut 引数より値を返す

判断系の関数が `&mut` で結果を書き込むのは禁止。値(または小さな構造体)を返す
(→ procedure-style.md)。

```rust
// ❌ 出力引数
fn resolve_options(manifest: &Manifest, out: &mut Vec<Option>) { /* ... */ }

// ✅ 戻り値
fn resolve_options(manifest: &Manifest) -> Vec<Option> { /* ... */ }
```

## 許容される mut

- モジュールが本来管理する状態そのもの(`&mut self` のサービス、
  インスタンステーブルなど)。ただしその関数内でも、値の組み立て・判断部分は
  上記のとおり不変で書き、mut に触るのは最後の「書き」だけにする
  (読み→判断→書き。→ procedure-style.md)
- 局所的な読み込みバッファ(`let mut buf = String::new(); file.read_to_string(&mut buf)`)
  のように、API が要求し関数内で完結するもの
- iterator で書くと明らかに読みにくくなる場合。その場合も mut のスコープは
  ブロックで最小に閉じる

```rust
// ✅ mut をブロックに閉じ込め、外に出るのは不変束縛
let index = {
    let mut m = HashMap::new();
    for entry in entries {
        m.entry(entry.kind).or_insert_with(Vec::new).push(entry);
    }
    m
};
```
