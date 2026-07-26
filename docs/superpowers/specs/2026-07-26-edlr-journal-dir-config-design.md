# edlr Journal ディレクトリ設定 設計書

2026-07-26 承認。Journal ディレクトリが自動検出できない環境で edlr が事実上起動しない問題を、
アプリ設定ファイルと設定 UI で解決する。

## 背景

`core/src/config.rs` の `default_journal_dir` は Proton の既定パス
`$HOME/.steam/steam/steamapps/compatdata/359320/pfx/...` のみを探索する。
Elite Dangerous がセカンダリ Steam ライブラリ(`libraryfolders.vdf` に登録された
`/mnt/game/SteamLibrary` など)にインストールされている環境ではこれが見つからず、
`core/src/bin/edlr.rs:52` が `std::process::exit(1)` で即死する。

このとき Tauri のウィンドウ自体は開くが、デーモンが居ないため WebSocket が繋がらず
何も表示されない。ユーザーからは「起動しない」ように見える。回避策は
`EDLR_JOURNAL_DIR` 環境変数の手動指定しかなく、UI から設定する手段が存在しない。

なお本設計の前に、`ui/src-tauri` をルート Cargo workspace のメンバーへ統合済み
(空の `[workspace]` を削除し `members` に追加)。新クレートも同じ workspace に属する。

## 決定事項

| 論点 | 決定 |
|---|---|
| 設定の持ち方 | 設定ファイルで明示指定。自動検出は現状維持 |
| journal_dir 未設定時のデーモン | **即死を維持**。Tauri 側が設定を持ち、値を決めてから spawn する |
| 設定 UI の形 | 常設の Settings ページに統一(未設定時はそこへ誘導) |
| 設定変更の反映 | Tauri がデーモンを自動再起動 |
| 優先順位 | 設定 > 自動検出 |
| 非 Tauri 環境 | 読み取り専用表示 |

デーモンの journal_dir 決定ロジック(CLI 引数 → 自動検出 → 無ければ即死)は**一切変更しない**。
設定ファイルを読むのは Tauri 側だけであり、デーモンは従来どおり `--journal-dir` しか知らない。

## クレート構成

新クレート **`edlr-config`**(リポジトリ直下 `config/`、`core/` や `drivers/` と同階層)。

`core/src/config.rs` の内容を丸ごと移設する。`default_journal_dir` /
`default_config_subdir` / `config_subdir` と既存の 9 テストはすべて `std` のみに
依存する純粋関数であり、移設は機械的。

- `edlr-config` の依存: `serde` / `serde_json`(`AppConfig` 用)。dev-dependencies に `tempfile`
- `edlr-core` は `core/src/lib.rs:1` を `pub use edlr_config as config;` に差し替える。
  これで `edlr_core::config::default_journal_dir(...)` も `crate::config::config_subdir(...)` も
  呼び出し側は無変更で通る
- `ui/src-tauri` は `edlr-config` に直接依存する

**この構成を採る理由**: 検討した代替案は 2 つある。

- **`ui/src-tauri` が `edlr-core` に依存する**: 重複はゼロだが、設定ファイルを読むのは
  Tauri だけなのに tokio / axum / wasmtime を不要に link することになる
- **`ui/src-tauri` に自前の config モジュールを置く**: 依存は最軽量だが、XDG 解決ロジックが
  二重管理になる。デーモンは同じ規則で `plugins/` と `settings/` を配置しているため、
  片方だけ変更すると `config.json` が別の場所に落ちる

今回のバグ自体が「パス解決が一箇所にハードコードされていた」ことに起因しているため、
ここを二重化しない。クレート抽出は、前者のビルド肥大なしに後者のドリフトリスクを消せる。

副次的な利点として、`plugins/` `settings/` `grants/`(capability 設計で追加予定)
`config.json` の 4 つすべてが単一の情報源から解決されるようになる。

## 設定ファイル

**パス**: `<config-base>/edlr/config.json`。`config-base` の解決は既存の `config_subdir` と同一規則
(`$XDG_CONFIG_HOME` があればそれ、無ければ `<home>/.config`、home も無ければ `.`)。

**形式**:

```json
{ "journalDir": "/mnt/game/SteamLibrary/steamapps/compatdata/359320/pfx/drive_c/users/steamuser/Saved Games/Frontier Developments/Elite Dangerous" }
```

**新規 API**:

```rust
pub fn config_file_path(xdg: Option<&Path>, home: Option<&Path>) -> PathBuf

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig { pub journal_dir: Option<PathBuf> }
```

**読み込み**: ファイルが存在しなければ `Ok(AppConfig::default())`。
**JSON のパースに失敗した場合は `Err` を返す**(既定値で黙って倒さない)。

これは `SettingsStore::effective`(`core/src/plugin/settings.rs:135`)が壊れた JSON を
黙って defaults に倒すのとは意図的に異なる方針である。`config.json` はユーザーが手で
編集しうる唯一のファイルであり、黙って倒れると「設定したのに反映されない」という
本設計が解決しようとしている症状そのものを再現してしまうため。

**書き込み**: `create_dir_all` の後、tmp ファイル + `rename` による atomic write
(`settings.rs:206` と同じ手口)。

## Tauri 側

### journal_dir の決定

純粋関数として切り出し、単体テスト可能にする:

```rust
fn resolve_journal_dir(env: Option<PathBuf>, config: Option<PathBuf>) -> Option<PathBuf>
```

優先順位は `EDLR_JOURNAL_DIR`(既存。デバッグ用に残す)→ `config.json` → `None`。
`None` の場合は `--journal-dir` を**渡さない**ため、デーモンが従来どおり自動検出を行う。
これにより「設定 > 自動検出」が成立し、自動検出が当たる環境では設定不要のままとなる。

### デーモンハンドルの保持

現在 `ui/src-tauri/src/main.rs:55` は `Child` をローカル変数に持ち、クロージャ内で
`RunEvent::Exit` 時に kill している。設定保存時に再起動する必要があるため、
`Arc<Mutex<Option<Child>>>` にして Tauri の managed state に載せ替える。
終了時の kill は同じ state を参照する。

kill + re-spawn は **`restart_daemon` 一関数に集約する**(理由は「スコープ外」を参照)。

### IPC コマンド

`ui/src-tauri/src/main.rs:56` は現在 `tauri::Builder::default().build()` のみで
`invoke_handler` を持たないため、新設する。デーモン未起動時は WebSocket が使えず
既存の RPC 経路では設定できないため、IPC が必須となる。

| コマンド | 役割 |
|---|---|
| `get_config` | `journalDir` と `daemonManaged`(Tauri が spawn した子かどうか)を返す |
| `set_journal_dir(path)` | `is_dir()` 検証 → 保存 → デーモン再起動 |
| `pick_journal_dir` | `tauri-plugin-dialog` のディレクトリ選択ダイアログを開く |

### 外部起動デーモンの扱い

`main.rs:11` の既存方針どおり、既にデーモンが動いていれば Tauri は spawn も kill もしない。
この場合 managed state は `None` で再起動できないため、`set_journal_dir` は
**保存のみ行い再起動はせず**、`daemonManaged: false` を返す。UI は
「設定は保存しました。外部で起動中のデーモンには自分で反映してください」と表示する。
他人のプロセスを勝手に殺さないという既存の設計判断を維持する。

### 再起動の手順

古い子を `kill` + `wait`(ゾンビ回収)してから、新しい引数で spawn する。
spawn に失敗した場合も**設定は保存済みのまま**エラーを返す。保存をロールバックすると
ユーザーが入力した正しい値まで失われるため。

## フロントエンド

`@tauri-apps/api` を `ui/frontend` の依存に追加し、`lib/tauri.ts` に薄いラッパを置く:

```ts
export function isTauri(): boolean   // window.__TAURI_INTERNALS__ の有無
export async function invoke<T>(cmd: string, args?): Promise<T>
```

`pages/Settings.tsx` を新設し、`App.tsx:6` の `TABS` に `"Settings"` を追加する。
既存 3 ページ(Dashboard / Logs / Plugins)と同じ構造を踏襲する。

**未設定時の誘導**: `App` が起動時に `get_config` を呼び、`journalDir` が未設定なら
**初期タブを Settings にする**。専用のセットアップ画面やバナーは設けない。

**非 Tauri 環境**: この frontend はデーモンの `--ui-dir`(`core/src/bin/edlr.rs:26`)経由で
ブラウザからも開ける。ブラウザには `window.__TAURI_INTERNALS__` が無いため
`isTauri()` が false となり、入力欄と保存ボタンを `disabled` にして
「デスクトップアプリから変更してください」を表示する。現在値も IPC 経由でしか
取得できないため表示しない。vitest / jsdom も同じ経路を通るので、テストが自然に書ける。

## エラー処理

| 状況 | 挙動 |
|---|---|
| config.json が壊れている | `load` が `Err` → Settings ページにパースエラーを表示。既定値で黙って上書きしない |
| 指定パスが存在しない | `set_journal_dir` が `is_dir()` で弾き、保存せずエラー返却 |
| デーモン再起動に失敗 | 設定は保存済みのままエラー表示 |
| デーモンが外部起動 | 保存のみ実施し `daemonManaged: false` を返す |

## テスト方針

- **`edlr-config`**: 移設した既存 9 テストがそのまま通ること。加えて `AppConfig` の
  新規テスト — ファイル無しで `Ok(default)`、正常な JSON の読み込み、壊れた JSON が `Err`、
  atomic save、`config_file_path` の XDG 分岐
- **`ui/src-tauri`**: `resolve_journal_dir(env, config)` の優先順位を純粋関数として単体テスト。
  既存の `daemon.rs::resolve_edlr_bin` が同じ手口を取っているため踏襲する
- **`ui/frontend`**: `Settings.tsx` を `isTauri()` true / false 両方で。`invoke` はモックし、
  保存成功・パス不正・再起動失敗の 3 経路を確認する

IPC コマンド本体とデーモン再起動は実プロセスに触れるため単体テストせず、
純粋関数部分を切り出してそちらで担保する(既存 `daemon.rs` と同じ考え方)。

## スコープ外

### サイドカー capability 導入前に必須となる前提条件

ドライバにサイドカー起動権限を与える設計を進める場合、**その前にデーモンの
graceful shutdown が必須**となる。理由は以下のとおり。

`ui/src-tauri/src/main.rs:62,78` は `Child::kill()` を使っており、Unix では
SIGKILL であって捕捉できない。またデーモン側にはシグナルハンドラが一切存在しない
(`core/src/` に `signal` / `ctrl_c` / `SIGTERM` の該当なし)。

この状態でドライバがサイドカーを spawn すると、サイドカーはデーモンの子となる。
SIGKILL はデーモンだけを殺すため、サイドカーは孤児化して init に再ペアレントされ
生き残る。本設計は kill 経路を「アプリ終了時のみ」から「終了時 + 設定変更時」へ
増やすため、**設定を変更するたびにサイドカーが 1 セットずつ残留する**ことになる。

対処の方向は、デーモンを独自プロセスグループ(`CommandExt::process_group`)に置き、
Tauri が `SIGTERM` をグループへ送って猶予付きで待ち、期限超過時のみ `SIGKILL` を送り、
デーモン側は SIGTERM でサイドカーを畳んでから終了する、という形になる。

本設計ではサイドカーがまだ存在せず実害がないため実装しない。代わりに kill + re-spawn を
`restart_daemon` 一関数へ集約し、将来の変更箇所を 1 つに絞る。

### その他

- `libraryfolders.vdf` のパースによる Steam ライブラリ全走査(自動検出は現状の Proton 既定パスのみ)
- デーモン側での設定ファイル読み込み(読むのは Tauri のみ)
- journal_dir 以外のアプリ設定項目
- journal_dir 変更をデーモン再起動なしで反映する仕組み
