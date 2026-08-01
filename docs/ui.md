# UI(ブラウザ版 / Tauri アプリ)

## 起動方法

    # デーモン(WS サーバ込み)を起動
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH>

    # ブラウザ版(開発)
    cd ui/frontend && pnpm install && pnpm dev   # http://localhost:5173

    # デーモンに静的配信させる場合
    cd ui/frontend && pnpm build
    cargo run -p edlr-core --bin edlr -- --journal-dir <PATH> --ui-dir ui/frontend/dist

    # Tauri(要 libwebkit2gtk-4.1-dev ほかシステム依存)
    cd ui/src-tauri && cargo tauri dev

Tauri アプリはデーモン未起動なら自動で spawn し、終了時に道連れで止める。
既に起動済みのデーモンには手を出さない。`EDLR_BIN` / `EDLR_JOURNAL_DIR` で
上書き可。

## Journal ディレクトリの設定(Tauri アプリ)

`edlr` バイナリ自身は `--journal-dir` を渡さない限り、Proton の既定パスを
自動探索する([cli.md](cli.md) 参照)。Steam のセカンダリライブラリにゲームを
入れている場合などはこの既定パスに当たらず、探索は失敗する。その場合デーモンは
フォールバックディレクトリ(`$XDG_DATA_HOME/edlr/journal`、`XDG_DATA_HOME`
未設定なら `~/.local/share/edlr/journal`)を作成して起動し、そこを監視する
(journal ファイルが実際に置かれる場所とは限らないので、ゲームを検出させたい
場合は Journal ディレクトリを明示的に設定するのが本筋)。

Tauri アプリはこれを設定ファイルで補う:

- 設定ファイルは `$XDG_CONFIG_HOME/edlr/config.json`(`XDG_CONFIG_HOME`
  未設定なら `~/.config/edlr/config.json`)。`journalDir` キー
  (文字列 or 省略)を持つ
- Settings 画面から Journal ディレクトリを選択・保存できる。保存すると
  Tauri が spawn したデーモンを保存先ディレクトリで再起動する
  (外部起動のデーモンを掴んでいる場合は保存のみで再起動はしない)
- `journalDir` が未設定なら、デーモンには `--journal-dir` を渡さず
  Proton 既定パスの自動探索に委ねる。一度設定した後で自動探索へ戻したい場合は
  Settings 画面の「自動検出に戻す」を使う(`journalDir` を消してデーモンを
  再起動する)
- デーモンの起動自体に失敗した場合(バイナリが見つからない等)は Settings
  画面にその理由を表示する。外部起動のデーモンが居るケースとは区別され、
  保存や「自動検出に戻す」で再度起動を試みる
- 環境変数 `EDLR_JOURNAL_DIR` が設定されている場合はそちらが常に優先される
  (spawn 時・Settings 画面での再起動時・Settings 画面の表示のすべてで
  同じ実効値になる)。設定ファイルに値があっても `EDLR_JOURNAL_DIR` が
  勝つので、Settings から保存しても実際の反映先は変わらない
