# edlr ui

デーモンに WebSocket で接続する GUI クライアント。

- `frontend/` — React + TypeScript + Vite の SPA(Logs / Plugins / Dashboard)
- `src-tauri/` — Tauri 2 の薄い皮(ウィンドウ表示のみ)

## 開発

    cd frontend && pnpm install && pnpm dev      # http://localhost:5173
    pnpm test                                    # vitest

デーモン側は `cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>` で起動しておく
(WS 既定: ws://127.0.0.1:8137/ws)。ビルド成果物をデーモンに配信させる場合は
`pnpm build` 後に `--ui-dir ui/frontend/dist` を付ける。
