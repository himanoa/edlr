# edlr — EliteDangerousLogRouter

Elite Dangerous の Journal / Status.json を監視し、イベントをドライバとプラグインへ配るルーター。
設計の全体像は [spec.md](spec.md) を参照。

## 構成

- `core/` — Rust 製カーネル。Journal tail(inotify + ポーリング常時併用)、JSON Lines パース、
  Status.json 監視、broadcast によるイベント配信。バイナリ名 `edlr`
- `drivers/` — 特権 capability を持つドライバ層(http / channel、現在はスケルトン)
- `ui/` — GUI クライアント。`frontend/`(React + Vite の SPA: Logs / Plugins / Dashboard)と
  `src-tauri/`(Tauri 2 の薄い皮)。デーモンとは WebSocket(既定 `ws://127.0.0.1:8137/ws`)で通信

## 使い方

    cargo run -p edlr-core --bin edlr -- --journal-dir <JournalディレクトリのPATH>

`--journal-dir` 省略時は Proton の既定パスを探索する。イベントは 1 行 1 JSON で stdout に流れる。

## UI

    # デーモン(WS サーバ込み)を起動
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

    # ブラウザ版(開発)
    cd ui/frontend && pnpm install && pnpm dev   # http://localhost:5173

    # デーモンに静的配信させる場合
    cd ui/frontend && pnpm build
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH> --ui-dir ui/frontend/dist

    # Tauri(要 libwebkit2gtk-4.1-dev ほかシステム依存)
    cd ui/src-tauri && cargo tauri dev
