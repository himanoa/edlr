# filesystem 権限の file target 追加

fs 権限はいまディレクトリ単位でしか承認できない。単一ファイルだけを
プラグインに読ませたい(例: Status.json の監視)場合にフォルダ丸ごと
渡すのは過剰なので、`[[filesystem]]` にファイル単位の指定を追加する。

## マニフェスト(TOML)

```toml
[[filesystem]]
name = "status"
reason = "Status.json を監視する"
mode = "read"
target = "file"   # 省略時 "directory"
```

- `target = "file" | "directory"`、`#[serde(default)]` で `directory`。
  既存 manifest は無変更で動く
- `mode` は直交のまま(file + read-write も宣言可。用途はエクスポート先
  ファイルなど)
- 実パスは従来どおり manifest に書けない — ユーザーが UI で選ぶ

## core

### 型(capability/request.rs)

- `FilesystemTarget { Directory, File }` を追加(`kebab-case`、
  `as_str()` あり — FilesystemMode と同じ作法)
- `FilesystemRequest` に `#[serde(default)] pub target: FilesystemTarget`

### fingerprint / grants

- `target` を fingerprint に含める(**file のときだけ**畳み込む — directory
  で無条件に足すと、導入前に承認された既存 grants が全部失効してしまう)。
  `directory` → `file` に変えた manifest は既存の staleGrant 機構で自動的に
  再承認要求になる
- grants ストアの形式は変更なし(path にファイルパスが入るだけ)

### runtime / host

- `FsRuntimeEntry` に `#[serde(default)] pub target: String` を追加
  (空文字/欠落は directory 扱い — 旧バッファ互換)
- `resolve_root` の解決結果に target を含め、file ルートでは host 側
  (ドライバ呼び出しの手前)で:
  - `read` / `read_range` / `stat`: `path == ""` のみ許可、非空は
    `invalid-path`
  - `list`: `invalid-path`
  - `write` / `append`: mode が read-write かつ `path == ""` のみ許可
    (対象ファイルへの原子的上書き/追記)。`delete` は拒否
- WIT は無変更。プラグインは `fs::read("status", "")` で読む
- `edlr_driver_fs` は**無変更**: host 側で承認済みファイルパスを
  「親ディレクトリ + ファイル名」に分解して既存の `FsDriver` API に渡す
  (パス検証・サイズ上限・原子的書き込みをそのまま流用)

## UI

- RPC 応答(plugin/driver 両方)の filesystem エントリに `target` を追加
- `FilesystemSection.tsx`: file のときラベルを「ファイル」、ピッカーを
  `pick_file` に切り替え。警告文も「このファイルを読み取れます」等に出し分け
- Tauri: `pick_file` コマンドを登録(実装は main.rs にある既存の
  pick_file 呼び出しを流用)

## テスト

- core: resolve の純粋テスト(file ルートで path 非空 → invalid-path、
  list 拒否、write の mode 判定、旧 JSON バッファのデフォルト directory)、
  manifest パース(target 省略時 directory / 不正値を弾く)、fingerprint
  が target の変更で変わること
- UI: FilesystemSection の file/directory 出し分けテスト

## スコープ外

- glob / 複数ファイル指定
- ファイル用の新 WIT 関数
- シンボリックリンク追跡ポリシーの変更(既存 FsDriver の検証に従う)
