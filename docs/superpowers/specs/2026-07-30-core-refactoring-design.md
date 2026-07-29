# core リファクタリング設計(挙動不変)

日付: 2026-07-30
状態: レビュー待ち

## 目的

挙動を一切変えずに、core crate の構造を改善する。達成したい目標:

1. 全容を把握しやすくする(1ファイル・1関数を小さく)
2. 手続き的な深いネストを減らし、判断を関数として分離する
3. 単一責任原則を守る(特に `Registry` の神オブジェクト解消)
4. trait ベースの DI で純粋なユニットテストを増やす
5. burntsushi/ripgrep のような「機能名モジュール + 依存方向で層を守る」構成を参考にする

## スコープ

- **対象**: core crate 全体(巨大ファイル上位: `plugin/registry.rs` 3417行、
  `plugin/manifest.rs` 3206行、`server.rs` 1855行、`driver/registry.rs` 1723行、
  `plugin/host.rs` 1482行、`plugin/runner.rs` 1406行)
- **対象外**: drivers/ 配下(1000行級はあるが別タスク)、ui/、config/
- **crate 分割はしない**。core crate 内のモジュール再編に留める
- **エラー型のスタイルは現状維持**(enum + 手書き Display。thiserror 導入はスコープ外)

## 目標アーキテクチャ

レイヤーディレクトリ(domain/ ports/ など)は作らない。ripgrep・cargo など
Rust の主要プロジェクトに倣い、**機能名モジュール + 依存方向**で層を表現する:

```
core/src/
├── manifest/    # TOML → Manifest のパースと全体整合の検証(純粋。I/O は load_manifest だけ端に)
├── capability/  # capability の要求と承認(純粋 + ディスク実装)
│   ├── request.rs      # 各種 Request 型と検証(manifest.rs から移動)
│   ├── fingerprint.rs  # 要求内容のハッシュ(再承認の要否判定)
│   └── grants.rs       # GrantState・失効判定・Storage trait・ディスク実装
├── settings/    # 設定の検証・マージ(純粋)+ Storage trait + ディスク実装
├── schedule/    # 発火計算(純粋)+ 永続化
├── rpc/         # RPC 解釈・JSON 整形(純粋関数群)
├── journal/     # 既存構成を踏襲(discovery/parser/position/tailer)
├── registry/    # プラグイン・ドライバの facade と各サービス(命令的)
├── runner/      # プラグインスレッドとイベントループ(命令的)
├── host/        # wasmtime 配線(命令的)
└── server/      # axum/WS。rpc/ を呼ぶだけの薄い層(命令的)
```

「capability」は要求(manifest で宣言)と承認(ユーザーが UI で許可、
fingerprint が変わると失効)を対で扱う1概念なので、grant を独立モジュールに
せず capability のサブモジュールに置く。manifest はこれらの型を use して
組み立てる側に回り、依存方向は `manifest → capability`(両方純粋)。

「命令的」と付けたモジュールは**副作用の実行を担当する場所**
(ディスク永続化・Mutex・プロセス起動停止・スレッド・チャネル・wasm 呼び出し・
ネットワーク)。時間がかかる・失敗しうる・順序が意味を持つ操作をここへ集める。
ただし汚くてよい場所ではなく、「手続きを綺麗にする作法」(後述)を適用して、
判断は純関数に抽出し、命令的関数には短い実行手順の羅列だけを残す。

規約は2つ:

1. **純粋モジュールは命令的モジュールを import しない**。`manifest`/`rpc` 等から
   `runner`/`server`/`std::fs` が見えたらレビューで弾く
2. **trait は使う機能のモジュールに置く**(`capability::GrantStorage` など)。
   中央の `ports/` ディレクトリは作らない

## trait DI

境界は4本から始め、必要が実証されたときだけ増やす:

| trait | 置き場所 | 既存の実装者 | モックで純粋テストになるもの |
|---|---|---|---|
| `capability::GrantStorage` | capability/ | `GrantsStore` | grant 遷移・fingerprint 検証の全パターン |
| `settings::Storage` | settings/ | `SettingsStore` | 値の検証・マージ・secret の write-only 規則 |
| `registry::ProcessControl` | registry/ | `ProcessDriver` | sidecar start/stop/restart の状態判定、取消と起動の競合系 |
| `registry::BusPort` | registry/ | `edlr_driver_channel::Bus` | select options 解決、bus grant 反映 |

- 時刻は trait にしない。**純関数が `now: Instant`(または `DateTime`)を引数で
  受ける**(Firezone/str0m の sans-IO 流。trait より単純で決定論テストに十分)
- DI の形は generics: `struct GrantService<S: Storage, P: ProcessControl>`。
  公開面は type alias(後述)でジェネリクスを隠す
- モックは各モジュールの `#[cfg(test)] mod test_support` に手書き。
  mockall 等のマクロ crate は導入しない
- wasmtime の `Store` などモックしても意味のない部分は具象のまま

## 手続きを綺麗にする作法

新しい機構・統一 Effect 言語・中央 enum は導入しない。原則は
「**判断と実行を分ける**」の一点で、手段は普通の関数抽出:

1. **判断は関数に抽出する**: 手続き中の `if`/`match` の塊は名前のついた純関数
   (入力は値、出力は値)に切り出す。抽出した関数がそのまま純粋テストの対象になる
2. **命令的関数は短く・直線的に**: 目安1関数1画面(〜40行)、ネスト2段まで。
   深くなったら early return・ガード節・関数抽出で平らにする
3. **読み→判断→書き の順に整える**: 読み書きが交互に出てくる手続きは、
   先に読む・真ん中で判断・最後に書く順に並べ替える。ロック取得も
   「読みの前・書きの前」に自然と整列する
4. **判断結果が複数あるときは小さな構造体で返す**: `runner::LoopAction` が既に
   やっている形。「そうすると綺麗になる場所でだけ」使い、全操作に義務付けない

先行事例: この方向性は Rust ネットワーク界隈の sans-IO
(quinn-proto / quiche / str0m / Firezone snownet)で本番実証されている。
edlr の `next_action`/`LoopAction` はその縮小版であり、本リファクタは
「runner が局所的にやっていることを core 全域の作法にする」ものと言える。

## 進め方(サブシステム単位で一気通貫)

各フェーズ完了時に `cargo test --workspace` 全パス + その領域は
「分割・trait 化・関数分離」まで済んだ状態にする。

| Phase | 領域 | 主な作業 |
|---|---|---|
| 0 | 共通語彙 | 4 trait を既存具象型から逆算して定義し、既存型に impl を付けるだけ(挙動不変が自明) |
| 1 | manifest + capability | テストをファイル分離。capability の Request 型・検証・fingerprint を `capability/` へ移動し、manifest はパースと全体整合の検証だけに。ほぼ純粋なので構造整理が主 |
| 2 | rpc + server | 着手前に代表 RPC 応答の pin テストを追加。巨大 match をメソッド単位の小関数(params 解釈 → Registry 呼び出し → JSON 整形)に分解、`*_result_json` 群を `rpc/render.rs` へ。server は WS/HTTP 配線だけ残す |
| 3 | schedule + settings + capability::grants | 判定ロジックを純関数に抽出、Storage trait 越しに永続化。モックによる純粋テストをここで初めて追加 |
| 4 | registry(plugin + driver 同時) | 神オブジェクト解体の本丸。`GrantService` / `SidecarService` / `FilesystemService` / `BusService` / `ThreadSupervisor` に分割し、`Registry` は薄い facade に。plugin/driver の同型コードをジェネリック共通化。runner 向けの境界(`ThreadSupervisor` の口)をここで確定 |
| 5 | runner + host | ループ判定の関数抽出を拡大(`fire_all_due`・shutdown 系)、wasmtime 配線と `HostCtx` の分離 |
| 6 | 仕上げ | journal 等の中規模ファイルを同じ作法に揃える。温存していた旧パス `pub use` を一括削除し、`use` 文を新パスへ置換。モジュールドキュメント整備 |

順序の根拠: 1→2 は依存が浅く低リスクで作法のひな形を作る。3 で trait DI の
パターンを確立してから最難関の 4 に挑む。4 と 5 の相互依存は、Phase 0 の語彙と
Phase 4 での `ThreadSupervisor` 境界確定で吸収する。

## 既存テストを壊さない工夫

原則: **テストは凍結、コンパイルを通すための機械的追従だけ許可**。

1. **旧パスの `pub use` 温存**: モジュールを動かしても旧パス
   (`crate::plugin::registry::Registry` 等)を `pub use` で残す。統合テスト
   13本と unit テストの `use` 文を書き換えずにコンパイルが通る状態を各フェーズの
   ゴールにする。旧パス削除は Phase 6 で一括(そのコミットは use 文置換のみ)
2. **型名の互換を type alias で維持**:
   `pub type Registry = GenericRegistry<DiskGrantStore, ProcessDriver>;` を旧名で
   残す。コンストラクタを変えたい場合も旧シグネチャの `new` は残し、
   新しい口は `with_deps` 等で足す
3. **move-only コミットと logic コミットの分離**: 1コミット = 移動のみ か
   ロジック変更のみ。移動コミットは `git diff --color-moved=dimmed-zebra` で
   ほぼ全行 moved 表示になることを確認してからコミット
4. **テストファイルの diff 監視**: 各フェーズの完了条件に「`core/tests/` 配下と
   `#[cfg(test)]` ブロックの diff が空、または import 行のみ」を含める。
   アサーション・テストデータ・テスト名に触る差分が出たら挙動変更の兆候として戻す
5. **フェーズごとのゲート**: フェーズ途中でもコミット単位で
   `cargo test --workspace` + `cargo clippy` を全パスさせる
6. **pin テストによる追加防衛**: Phase 2 の着手前に代表的な RPC 応答の生 JSON を
   捕捉するテストを数本追加する(テストの追加は凍結原則と矛盾しない。終了後も残す)

## テスト戦略のまとめ

- 既存テスト(実ディスク・実スレッド)= 挙動の錨。統合テストとして残す
- 新規の純粋テスト = 分解した単位の仕様書。モック or 値の等値比較で書く
- 二層は役割が違うので、純粋テストを足しても既存テストは消さない

## 検討して採用しなかった案

- **crate 分割**(ripgrep 完全準拠): 効果最大だが差分も最大。モジュール再編で
  依存方向は表現できるため見送り
- **レイヤーディレクトリ**(domain/ ports/ runtime/): hexagonal 系の語彙は
  Rust 主要 OSS でほぼ使われておらず、機能名モジュールの方が慣習に合う
- **統一 Effect enum + 汎用 interpreter**: dry-run・監査ログが横断で得られるが、
  効果の結果が型消去され、中央 enum が全モジュールを知る集権点になる。
  必要になった操作だけ後から格上げできるので初手では採らない
- **mockall 等のモックマクロ**: ボイラープレート削減より可読性を優先し手書き
- **`Clock` trait**: `now` を引数で渡す方が単純(sans-IO 各実装の実例に一致)
