# プラグイン作成チュートリアル(Rust / TinyGo)設計

## 背景

`docs/plugins.md` は WIT world・manifest 全フィールド・RPC 仕様を並べた
リファレンスであり、「初めて書く人が上から順にやれば動く」導線が無い。
サンプルも Rust の `hello-logger` / `state-reader` と TinyGo の
`inara-uploader` が点在するだけで、段階を追って機能を足していく教材がない。

## ゴール

読者(このリポジトリの開発者ではなく、プラグインを書きたい人)が、
ゼロから 1 つのプラグインを育てて、`on-event` / 設定 / HTTP capability /
スケジュール / 終了フック / ドライバとの bus 連携まで到達できること。

## 題材

**`jump-log`** — FSDJump を拾って星系を記録し、集計を外へ出すプラグイン。
1 つのコードベースに章ごとに機能を積む。

| 章 | 積む機能 | 触る仕組み |
| --- | --- | --- |
| 2 | FSDJump を受けてログに出す | `init` / `on-event`、`host-log`、manifest 最小形、配置と起動 |
| 3 | `minDistance` 未満のジャンプを無視 | `[[settings]]`、`host-settings.get-all`、UI からの即時反映 |
| 4 | 星系情報を EDSM から引いてログに出す | `[[capabilities]]`、`driver-http`、承認フローと `permission-denied` |
| 5 | 60 秒ごとに集計を flush、停止時に最終 flush | `[[schedule]]`、`on-schedule`、`on-stop` の best-effort な性質 |
| 6 | 訪問先を自作ドライバへ publish し retain 値を読む | `[[bus]]`、`driver` world、`bus-host.emit`、`bus.publish` / `bus.get` |

HTTP の相手は EDSM の公開 API(`https://www.edsm.net/api-v1/system`)。
API キー不要、Elite 文脈で意味が通り、GET だけで完結して外部に副作用が無い。
`secret` 型は題材上必要にならないので、4 章の末尾に数行の参照のみ置く。

ドライバ `tracker` は最小構成: `visit` トピック(`retain = false`)を受け、
`last-system`(`retain = true`)へ `emit` するだけ。プラグイン側の
publish → retain 値の `bus.get` 読み返しが 1 章で閉じる。

## 成果物

### 文書

- `docs/plugin-tutorial-rust.md`
- `docs/plugin-tutorial-tinygo.md`

言語別に完全独立。それぞれ単体で上から下まで読める。`docs/plugins.md` は
「作った後に引く辞書」として残し、相互リンクを張る(チュートリアル →
リファレンス、リファレンス冒頭 → 「初めてなら先にこちら」)。

### コード(examples に完成形)

```
examples/plugins/tutorial-jump-log-rs/    examples/plugins/tutorial-jump-log-go/
examples/drivers/tutorial-tracker-rs/     examples/drivers/tutorial-tracker-go/
```

id を言語ごとに分けるのは、同じ id だと両方を同時にインストールできず
`install-examples.sh` の全件実行が衝突するため。4 件とも
`install-examples.sh` の対象に加える。

## 各章の型

1. この章で何ができるようになるか(1〜2 文)
2. コード差分(章の前後で増えた部分。全文再掲はしない)
3. ビルドして配置(章 2 で覚えたコマンドの再実行)
4. **動いたことをどう確認するか** — 何のログが出れば成功か、UI のどこを見るか
5. うまくいかないとき — その章で踏みやすい失敗と原因

4 と 5 を全章に置くのが眼目。手順書は「書いたが動かない、なぜ動かないかも
分からない」で止まるのが最も多い。

## 言語ごとの落とし穴(該当章に埋め込む)

**TinyGo**

- ビルド対象は `--wit-world plugin-guest`(`plugin` を指すとコンポーネント化が
  失敗する。標準ライブラリが WASI を import するため)
- `gen/` の再生成が要るのは `core/wit` を変えたときだけ
- `main` パッケージは `//go:wasmimport` を含むためネイティブでリンクできず、
  テストが書けない。判断を持つコードは `main` の外に置く
- `encoding/json` は reflect 差でネイティブと挙動が変わりうるので、
  `tinygo test -target=wasip1` でも走らせる

**Rust**

- `rustup target add wasm32-wasip2`
- `wit_bindgen::generate!` はパス指定なら WIT 変更に自動追随する
- `export!` マクロの位置

## 章 7 以降

- トラブルシュート: 呼び出し期限超過(3 回連続で `Disabled`)とトラップ
  (1 回で `Disabled`)の区別、world 不一致でロードに失敗する形、TOML の
  テーブルヘッダの落とし穴、`dropped` の読み方
- 次に読むもの: `docs/plugins.md` / `capabilities.md` / `drivers.md` / `ui.md`

章 0 の前提には、CLAUDE.md にある並列ビルド時の `cargo fetch` のような
このリポジトリの開発者向けの注意は入れない(読者はプラグイン作者である)。

## 検証方針

各章の最終状態を実際にビルドし、少なくとも `wasm-tools validate` を通す。
実行確認はダミー journal に FSDJump を書いて流せる範囲で行う。EDSM への
実通信と UI 操作は環境に依存するため保証せず、通せたかどうかを報告で明示する。
文書には確認できていないことを断定的に書かない。

## 非目標

- `driver-process`(サイドカー)・`driver-fs`・`[[dashboard]]` の解説。
  リンクのみに留める
- CI でのチュートリアル用サンプルのビルド検証
