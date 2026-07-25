# edlr モノレポ + 監視コア 設計書

2026-07-25 のブレインストーミングで承認された設計。上位仕様は `spec.md` を参照。

## スコープ

今回実装するのは **監視コア** まで:

- モノレポ(Cargo workspace)の骨組み
- `core` に Journal tail・JSON Lines パース・`Status.json` 監視・broadcast による内部イベント配信
- `drivers/` 配下は空実装スケルトン crate のみ(capability モデル設計後に実装)
- `ui/` はディレクトリ予約 + README のみ(spec の実装順序どおり、ブラウザ版ダッシュボード → Tauri は後段)

wasmtime プラグイン実行・WebSocket サーバ・GUI は今回のスコープ外。

## モノレポ構成

```
edlr/
├── Cargo.toml              # Cargo workspace(core と drivers/* をメンバに)
├── core/                   # crate: edlr-core — カーネル本体(lib + bin "edlr")
├── drivers/
│   ├── http/               # crate: edlr-driver-http    — 空実装スケルトン
│   └── channel/            # crate: edlr-driver-channel — 空実装スケルトン
├── ui/                     # README のみ(Tauri は後段)
└── docs/superpowers/specs/ # 設計ドキュメント
```

## core の内部構造

| モジュール | 責務 |
|---|---|
| `event.rs` | `Event` enum。`Journal { timestamp, event: String, raw: serde_json::Value }` と `Status { raw: serde_json::Value }`。生 JSON を保持し、型付けは下流に委ねる(ルーターは配るだけ、という設計思想に一致) |
| `journal/` | Journal ディレクトリ内の最新 `Journal.*.log` の特定、ローテーション追従、position 追跡による追記分の読み取り、JSON Lines パース(壊れた行はスキップしてログ) |
| `status.rs` | `Status.json` の変更検知と読み取り。書き込み途中の空ファイル・不完全 JSON はリトライ。同一内容は重複配信しない |
| `watch.rs` | ファイル変更検知。下記「監視方式」参照 |
| `router.rs` | `tokio::sync::broadcast` で `Arc<Event>` を pub/sub 配信 |
| `bin/edlr.rs` | デーモン本体。`--journal-dir` で監視対象を指定(未指定時は Proton / ネイティブの既知パスを探索)。当面はイベントを stdout に JSON で流して動作確認する |

### 監視方式(spec 必須要件への対応)

**inotify(notify クレート)+ 常時インターバルポーリング(既定 1 秒)のハイブリッド。**

- inotify イベントが来たら即座に読む
- 来なくてもタイマーで定期的に読む
- 読み取りは position 追跡で冪等なので、「inotify が壊れているか」を判定するロジック自体が不要

spec の必須要件「ポーリングへのフォールバック経路を最初から設計に組み込む」を、フォールバックではなく常時経路として満たす。

### Journal ディレクトリの既定探索パス

1. `--journal-dir` 引数(最優先)
2. Proton: `~/.steam/steam/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous`
3. 見つからなければエラー終了(メッセージでパス指定方法を案内)

## エラー処理

- 監視ループはファイル消失・パース失敗・一時的な読み取りエラーで死なない(ログして継続)
- broadcast の受信側 lag(RecvError::Lagged)は受信側の責務として許容

## テスト方針

- superpowers:test-driven-development に従い TDD で進める
- tempdir にフィクスチャ Journal を書き、追記・ローテーション・不完全書き込み(途中までの行)を再現する統合テスト中心
- 実ゲーム環境依存のパス探索はユニットテストで分離

## 今回スコープ外(spec.md の未決定事項に対応)

- ドライバの capability モデル
- プラグインマニフェスト・アーカイブ形式
- チャネルドライバの通信セマンティクス
- カーネル⇔GUI 間 WebSocket プロトコル
