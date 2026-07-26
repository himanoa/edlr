# edlr ファイルアクセス capability(driver-fs)設計書

2026-07-26 承認。プラグインが情報を保存・読み書きするための capability。プラグインは
`manifest.toml` で「この用途のディレクトリが要る」と宣言し、ユーザーが実ディレクトリを
割り当てて承認すると、そのディレクトリ配下に限ってファイルを読み書きできる。

`driver-http`(2026-07-26)・`driver-process`(2026-07-26)に続く 3 つ目の capability で、
両者と同じ「manifest 宣言 → ユーザー承認 → 呼び出し時照合」の型に載せる。

## 中核の方針

- **パスはプラグインが決められない**。プラグインが渡すのは要求の `name` と、その配下の
  相対パスだけ。ルートの実パスはユーザー設定由来で、プラグインからは観測できない
- **検証は呼び出しごと**。承認・取消は次の呼び出しから即座に効く(プラグイン再起動不要)
- **サンドボックス脱出を多層で防ぐ**。構文検証 → 正規化後の配下チェック → `openat2` による
  カーネルレベルの拘束、の 3 段
- **書き込みは原子的**(tmp + rename)。読み手が半端な内容を見ることはない
- 大きいファイルは `stat` + `read-range` で分割して扱う。単発 `read` の上限は
  ホスト側バッファの保護のためのものであり、扱えるファイルサイズの上限ではない

### 責務の分担

| 置き場所 | 内容 | 誰が決めるか |
|---|---|---|
| プラグインの `manifest.toml` の `[[filesystem]]` | 用途の**要求**(`name` / `reason` / `mode`) | プラグイン作者 |
| `<settings-dir>/<plugin-id>.filesystem.json` | 割り当てた**ディレクトリの実パス** | ユーザー(UI) |
| `<grants-dir>/<plugin-id>.json` | どの要求を承認したか | ユーザー(UI) |
| `drivers/fs` crate | パス検証・原子的書き込み・サイズ上限 | — |

`drivers/http` / `drivers/process` と対称の構造。`core` 側は既存の capability 機構に
`[[filesystem]]` を足すだけにする。

### 詐称不可の性質

`driver-http` / `driver-process` と同じく、プラグインは自分の ID も承認状態も引数で渡さない。
`HostCtx`(wasm インスタンスごとに固有)が保持する共有バッファからのみ、ルートパスと承認状態を
読む。未承認のエントリはそのバッファに**ルートパスを持たない**ため、承認前は書き込み先の情報
自体が存在しない。他プラグインのルートを騙ることもできない。

## マニフェストでの宣言

```toml
[[filesystem]]
name = "exports"
reason = "巡回した星系の一覧を CSV で書き出すため"
mode = "read-write"   # "read" | "read-write"

[[filesystem]]
name = "cache"
reason = "取得済みの星系データを再取得しないよう保持するため"
mode = "read-write"
```

- `[[filesystem]]` は複数書ける。`name` はプラグイン内で一意、`[a-z0-9-]+`
- **`path` は manifest に書けない**。実ディレクトリは必ずユーザーが UI で選ぶ
- `reason` は必須・空文字不可。承認画面に表示する。`capabilities` / `[[sidecar]]` と同じく
  trim して制御文字・ゼロ幅文字を拒否する(承認画面に描画される文字列とフィンガープリントの
  入力を byte 単位で一致させるため)
- `mode` は `read` / `read-write` のみ。未知の値はマニフェスト検証エラー

### マニフェスト検証エラー(そのプラグインのみロードしない)

- `name` の重複、`[a-z0-9-]+` にマッチしない `name`
- `reason` の欠落・空文字・不可視文字
- 未知の `mode`

## ユーザー設定

保存先は `<settings-dir>/<plugin-id>.filesystem.json`。通常の `[[settings]]`
(`<settings-dir>/<plugin-id>.json`)と別ファイルにするのは、`SettingsStore::update` が
manifest の `[[settings]]` に無いキーを間引く実装であり、同居させると設定保存のたびに
消えるため(`[[sidecar]]` と同じ理由)。

```jsonc
{
  "exports": { "path": "/home/himanoa/Documents/edlr-exports" }
}
```

- `path` が空なら未設定。承認できず、呼び出しは `not-configured`
- 保存時の検証:
  - 絶対パスであること
  - 実在し、ディレクトリであること(シンボリックリンクは解決したうえで判定)
  - **システム上重要なディレクトリそのものでないこと** — `/`、`/home`、`/etc`、`/usr`、
    `/var`、`/boot`、`/dev`、`/proc`、`/sys`、およびユーザーのホームディレクトリそのもの。
    承認画面での確認だけに頼らず、明らかな事故を 1 段止める(配下の任意のディレクトリは可)
- 検証に失敗した場合は何も書き込まない
- パス変更は再承認を要さない(フィンガープリントに含めない)。次の呼び出しから新しいルートを使う

## 付与(grants)

- 保存先: 既存の `<grants-dir>/<plugin-id>.json`。HTTP・サイドカーの承認と同じファイルに項目を足す
- **承認は `[[filesystem]]` エントリ単位**(`exports` は許すが `cache` は許さない、が可能)
- **既定は未承認**。未承認でもプラグインは通常どおり起動する。未承認のルートへの呼び出しは
  すべて `permission-denied`
- フィンガープリントに含めるもの: `name` / `reason` / `mode`。変われば自動失効し、UI が再承認を促す
- **ユーザーが選んだ `path` はフィンガープリントに含めない**
- **`path` 未設定のエントリは承認できない**。この検証は `Registry::set_filesystem_grant` 自身が
  強制する(UI の制約だけにしない — `driver-process` の `command` で同じ穴が最終レビューで
  見つかったため、最初から両方に置く)
- 承認・取消は即座に反映される

## WIT 追加

```wit
interface driver-fs {
  record entry {
    path: string,            // ルートからの相対パス
    size: u64,
    modified: option<u64>,   // Unix epoch 秒。取得できなければ none
  }

  variant driver-error {
    permission-denied(string),  // 未承認 / mode 違反
    not-configured(string),     // ディレクトリ未設定
    unknown-root(string),       // manifest にない root 名
    invalid-path(string),       // 脱出を含む不正なパス
    not-found(string),
    too-large(string),
    io(string),
  }

  read:       func(root: string, path: string) -> result<list<u8>, driver-error>;
  read-range: func(root: string, path: string, offset: u64, len: u32) -> result<list<u8>, driver-error>;
  stat:       func(root: string, path: string) -> result<entry, driver-error>;
  list:       func(root: string, prefix: string) -> result<list<entry>, driver-error>;
  write:      func(root: string, path: string, bytes: list<u8>) -> result<_, driver-error>;
  append:     func(root: string, path: string, bytes: list<u8>) -> result<_, driver-error>;
  delete:     func(root: string, path: string) -> result<_, driver-error>;
}
```

`world plugin` に `import driver-fs;` を追加する(ゲスト向けの `world plugin-guest` は
`include plugin` なので自動的に追随する)。

## パス検証

**この機能の中核。間違えるとサンドボックス脱出そのものになる。** 3 段で防ぐ。

### 1. 構文レベルの拒否

次のいずれかに当たるパスは、ファイルシステムに触る前に `invalid-path` で弾く:

- 空文字
- NUL・制御文字を含む
- `\` を含む
- 絶対パス(`/` 始まり)
- `..` または `.` を要素として含む
- 空要素を含む(`a//b`、末尾 `/`)

### 2. 正規化後の配下チェック

- ルートは設定時に一度 `canonicalize` し、その結果を基準にする
- 読み取り系(`read` / `read-range` / `stat` / `delete`)は対象パスを `canonicalize` し、
  正規化済みルートの配下にあることを確認する。`canonicalize` はシンボリックリンクを解決
  するため、**リンクで外を指していればここで落ちる**
- 書き込み系(`write` / `append`)は親ディレクトリに対して同じ検証を行う。`logs/2026-07.csv`
  のように途中のディレクトリが無ければ作るが、作る過程の各段でも配下チェックを行う

### 3. カーネルレベルの拘束(TOCTOU 対策)

検証と `open` の間にシンボリックリンクを差し替えられる余地を潰すため、Linux では
`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` でルート FD 相対に開く。`openat2` が
使えない環境(カーネル 5.6 未満、他 OS)では「正規化 + 配下チェック + `O_NOFOLLOW`」に
フォールバックする。フォールバック経路でも 1・2 段目は必ず通る。

## セマンティクス

- **書き込みは原子的**。同一ディレクトリに tmp ファイルを作り、`rename` で置き換える。
  読み手が半端な内容を見ることはない
- **`append` は原子的ではない**(追記オープン)。ログ用途では途中で切れても後ろに足されるだけ
  なので許容する
- `mode = "read"` のルートに対する `write` / `append` / `delete` は `permission-denied`
- `list` は `prefix` 配下を**再帰的に**列挙し、**ファイルのみ**を返す(ディレクトリ自体は
  含めない)。エントリ数の上限は 10,000 件で、超えたら `too-large`(呼び出し期限を食い潰さないため)
- `read` / `read-range` は 1 回 **8 MiB** まで(`driver-http` の `HTTP_MAX_BODY` と同値。
  ホスト側バッファの保護)。これを超えるファイルは `stat` でサイズを見て `read-range` で
  分割して読む
- `write` / `append` にホスト由来のサイズ上限は設けない。書くバイト列は既にゲストのメモリ上に
  あり、`PLUGIN_MEMORY_LIMIT`(64 MiB)が実質の上限になる
- `read-range` の `offset` がファイル末尾を超える場合は空のリストを返す(エラーにしない)
- ディレクトリ全体のクォータは設けない

## RPC 追加

既存の `{"type":"rpc", "id":…, "method":…, "params":…}` 形式に 3 メソッド追加し、
`plugins/list` を拡張する。

- `plugins/get-filesystem` `{"plugin": "<id>"}` → 下記の result
- `plugins/set-filesystem-config` `{"plugin": "<id>", "name": "<name>", "config": {"path": "..."}}`
  → 同形。検証エラーは `rpc-error` で、値は変更されない
- `plugins/set-filesystem-grant` `{"plugin": "<id>", "name": "<name>", "granted": <bool>}` → 同形
- `plugins/list` の各要素に `filesystem` を追加

result の形:

```jsonc
{
  "roots": [
    {
      "name": "exports",
      "reason": "巡回した星系の一覧を CSV で書き出すため",
      "mode": "read-write",
      "granted": false,
      "staleGrant": false,
      "config": { "path": "" }
    }
  ]
}
```

未知の `plugin` / `name` は `rpc-error`。

## UI

Plugins 画面の各プラグインカードに、capability / サイドカーと並ぶ「ファイルアクセス」
セクションを追加する。`[[filesystem]]` の無いプラグインには何も表示しない。

- 要求ごとに `reason` と mode バッジ(「読み取りのみ」/「読み書き」)
- ディレクトリ選択(既存の `pick_journal_dir` と同じネイティブピッカー。汎用の
  `pick_directory` コマンドに一般化して両方から使う)
- 承認トグル。`config.path` が空の間は無効。`checked` はサーバから返った `granted` だけで
  駆動し、楽観的更新をしない(`CapabilitySection` / `SidecarSection` と同じ流儀)
- 承認時の警告文は mode で出し分ける:
  - `read-write` → **「承認すると、このプラグインは選んだフォルダ内のファイルを読み取り・作成・
    上書き・削除できます」**
  - `read` → 「承認すると、このプラグインは選んだフォルダ内のファイルを読み取れます」
- 未承認時は「未承認 — このプラグインはファイルにアクセスできません」
- 失効時(`staleGrant`)は「要求が変わったため再承認が必要です」

## テスト方針

**パス脱出が中心。** ここは厚く書く。

- **構文レベル**: 絶対パス / `..` / `.` / 空要素 / 末尾 `/` / 空文字 / NUL・制御文字 / `\`
- **シンボリックリンク**: ルート内のリンクが外のファイルを指す、外のディレクトリを指す、
  途中のコンポーネントがリンク、リンクのループ — いずれも `invalid-path`
- **TOCTOU**: 検証後に対象をリンクへ差し替えるレースでルート外へ書けないこと
  (`openat2` 経路とフォールバック経路の両方)
- **書き込み**: 親ディレクトリの自動作成がルート配下に留まる、tmp + rename で読み手が
  半端な内容を見ない、`mode = "read"` からの `write` / `append` / `delete` が `permission-denied`
- **サイズ**: 8 MiB 超の `read` が `too-large`、`read-range` の `len` 上限、`offset` が
  ファイル末尾を超えた場合に空を返すこと、`list` の 10,000 件上限
- **grants**: 既定未承認、エントリ単位の承認、要求変更での失効、取消が次の呼び出しから
  効くこと、`path` 未設定では承認できないこと
- **設定**: 永続化、実在しないパス / ファイルを指すパス / システム重要ディレクトリの拒否
- **実 wasm 統合**: 未承認で `permission-denied`、承認後に書いて読み返せる、`read` モードでの
  書き込み拒否、脱出パスの拒否
- **RPC / UI**

## スコープ外

- ディレクトリ全体のクォータ(必要になってから)
- ファイル監視(変更通知)
- ファイルロック・排他制御(同一ファイルへの同時書き込みは最後の書き手が勝つ)
- ハンドルベースのストリーミング(`read-range` / `append` で代替)
- パーミッション・所有者・実行ビットの操作
- ディレクトリの削除・リネーム(ファイル単位の `delete` のみ)
- 複数プラグインが同じディレクトリを共有することの禁止
- プラグイン専用の私有ストレージ(承認不要で使える `<data-dir>/<plugin-id>/`)。
  今回の capability とは別物として、必要になったら改めて設計する
