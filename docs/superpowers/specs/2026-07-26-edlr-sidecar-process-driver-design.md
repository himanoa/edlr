# edlr サイドカープロセス capability(driver-process)設計書

2026-07-26 承認。spec.md の「重い処理(翻訳推論、TTS)は別プロセスとして HTTP 越しに呼び出す」構成を、
プラグインから扱えるようにする。プラグインがサイドカープロセスの起動を要求し、ユーザーが実行ファイルを
指定・承認すると、ホストがそのプロセスを起動・監視・確実に停止する。

## 中核の方針

- **API は命令的だが冪等**。プラグインは `ensure-started` / `stop` / `status` の 3 つだけを呼び、
  PID もプロセスハンドルも受け取らない。プロセスの所有権は最後までホストが持つ
- **argv はプラグインが決められない**。実行ファイルパスも引数もユーザー設定由来。承認したものと
  実際に走るものが必ず一致する
- **自動再起動はしない**。落ちたら `status` が `exited` を返すだけ。再起動はプラグインの再呼び出しか
  ユーザー操作で、ホストはレート制限だけを強制する
- **後始末はホストが保証**。デーモン停止・プラグイン無効化・承認取消・UI 操作のいずれでも
  プロセスグループごと停止し、孤児を残さない
- 通信は既存の `driver-http` を使う。サイドカーは HTTP サーバとして立ち、プラグインは
  `http://127.0.0.1:<port>` に喋る。新しい配管は作らない

### 責務の分担

| 置き場所 | 内容 | 誰が決めるか |
|---|---|---|
| プラグインの `manifest.toml` の `[[sidecar]]` | サイドカーが要るという**要求**、`args` / `port` の既定値提案 | プラグイン作者 |
| `<settings-dir>/<plugin-id>.sidecars.json` | `command` / `args` / `port` / `replicas` の**実値** | ユーザー(UI) |
| `<grants-dir>/<plugin-id>.json` | どのサイドカーを許可したかの**承認結果** | ユーザー(UI) |
| `drivers/process` crate | プロセスを起動・監視・確実に殺す能力そのもの | — |

`drivers/http` と対称の構造で、`core` 側は既存の capability 機構(manifest 宣言 → grants 承認 →
呼び出し時照合)に `kind = "process"` 相当を足すだけにする。

### 詐称不可の性質

`driver-http` と同じく、プラグインは自分の ID も設定も引数で渡さない。`HostCtx`(wasm インスタンスごとに
固有)から自分の supervisor ハンドルと自分の設定・grants だけを読む。他プラグインのサイドカーを操作したり、
他プラグインの承認を騙ることはできない。

## マニフェストでの宣言

```toml
[[sidecar]]
name = "tts"                   # [a-z0-9-]+、プラグイン内で一意
reason = "音声合成エンジンをローカルで動かすため"
args = ["--port", "{port}"]    # 既定値の提案。ユーザー編集可
port = 50021                   # 先頭ポートの既定値。ユーザー編集可
scalable = true                # replicas をユーザーに開放するか(既定 false)

[[sidecar]]
name = "translate"
reason = "翻訳モデルをローカルで動かすため"
args = ["serve", "--port", "{port}"]
port = 8080
```

- `[[sidecar]]` は複数書ける(種類の違う常駐を並べる)。`name` はプラグイン内で一意
- **`command` は manifest に書けない**。実行ファイルのパスは必ずユーザーが入力する
- `reason` は必須・空文字不可。承認画面に表示する
- `{port}` は起動時にそのインスタンスの実ポートへ展開される
- `scalable = true` のサイドカーだけ、ユーザーが `replicas` を 1 以上に設定できる(既定 1)
- `[[sidecar]]` 省略時は要求なし(従来どおり)

### マニフェスト検証エラー(そのプラグインのみロードしない)

- `name` の重複、`[a-z0-9-]+` にマッチしない `name`
- `reason` の欠落・空文字
- `port` が 1..=65535 の範囲外

## ユーザー設定

保存先は `<settings-dir>/<plugin-id>.sidecars.json`:

```jsonc
{
  "tts": { "command": "/usr/bin/piper", "args": ["--port", "{port}"], "port": 50021, "replicas": 2 }
}
```

通常の `[[settings]]`(`<settings-dir>/<plugin-id>.json`)と別ファイルにするのは、
`SettingsStore::update` が manifest の `[[settings]]` に無いキーをディスクから
間引く実装であり、同居させると設定保存のたびにサイドカー設定が消えるため。

- 未設定のキーは manifest の既定値にフォールバックする(`command` には既定値がない)
- `replicas` 個のインスタンスに `port, port+1, …, port+replicas-1` を採番する
- 設定変更時は当該サイドカーを停止する。次に `ensure-started` が呼ばれたら新しい設定で起動する

### 設定の検証エラー(`rpc-error` を返し、値は変更しない)

- `scalable = false` のサイドカーに `replicas > 1`
- `args` に `{port}` を含まないまま `replicas > 1`(全プロセスが同じポートを掴んで死ぬため)
- 同一プラグイン内でサイドカーのポート範囲が重なる
- 採番の結果 65535 を超える

## 付与(grants)

- 保存先: 既存の `<grants-dir>/<plugin-id>.json`。HTTP の承認と同じファイルに項目を足す
- **承認はサイドカー単位**(`tts` は許すが `translate` は許さない、が可能)
- **既定は未承認**。未承認でもプラグインは通常どおり起動する。未承認のサイドカーへの
  `ensure-started` / `stop` / `status` は `permission-denied` を返す
- フィンガープリントに含めるもの: `name` / `reason` / `args`(manifest の既定値)/ `port` / `scalable`。
  変わればハッシュ不一致で承認は**自動失効**し、UI が再承認を促す
- **ユーザーが入力した `command` はフィンガープリントに含めない**。パスの変更は再承認ではなく
  「設定変更 → 停止 → 次の `ensure-started` で新パスを起動」として扱う
- `command` が未設定のサイドカーは承認できない(UI の承認トグルはパス入力後に有効化)
- 承認・取消は即座に反映される。取消時は稼働中のサイドカーを直ちに停止する

### driver-http への暗黙許可

サイドカーが承認されている間に限り、そのサイドカーに**採番された全ポート**について
`http://127.0.0.1:<port>` を `driver-http` の許可リストへ暗黙に加える(`allowlist.rs` に暗黙エントリとして
合流させる)。承認が取消・失効した時点で暗黙許可も消える。隣接ポートや他ホストは許可されない。

この暗黙許可は **http capability の承認とは独立に効く**(`[[capabilities]]` を
一切宣言していないプラグインでも、承認済みサイドカーとは通信できる)。そのため
`capabilities_json` の形を `{"granted": bool, "hosts": [...]}` から
`{"hosts": [...]}`(= 実効的に許可されたホストだけを載せる)へ変更し、
`driver-http.send` は「空なら全部拒否、そうでなければ allowlist 判定」を見る。
承認状態の解決は `Registry` 側で行う。

## WIT 追加

```wit
interface driver-process {
  enum instance-state { running, exited }

  record instance {
    index: u32,                 // 0..replicas-1
    port: u16,                  // このインスタンスに採番されたポート
    state: instance-state,
    exit-code: option<s32>,     // exited のときのみ
  }

  variant driver-error {
    permission-denied(string),  // 未承認 / 承認が失効している
    not-configured(string),     // command 未設定など、ユーザー設定が未完了
    unknown-sidecar(string),    // manifest にない name
    rate-limited(string),       // 直近の spawn 試行から 1 秒未満
    spawn-failed(string),       // 実行ファイルが無い、権限が無い 等
  }

  ensure-started: func(name: string) -> result<list<instance>, driver-error>;
  stop: func(name: string) -> result<_, driver-error>;
  status: func(name: string) -> result<list<instance>, driver-error>;
}
```

`world plugin` に `import driver-process;` を追加する。ホストは常にこのインターフェースを提供し、
許可判定は各関数の実装内で行う。

## セマンティクス

- `ensure-started` は**冪等かつ非ブロッキング**。生きていないインスタンスだけを spawn し、直後の状態一覧を
  返す。`driver-http.send` と違って待たないため、`PluginInstance::CALL_DEADLINE`(2 秒)を圧迫しない
- **ヘルスチェックはしない**。`running` は「プロセスが生きている」だけを意味し、HTTP が listen 済みとは
  限らない。サイドカーの準備完了はプラグインが `driver-http` のリトライで判断する。ホストが待つと
  呼び出し期限を食い潰すため、意図的にこの分担にしている
- `stop` は当該サイドカーの全レプリカを停止する。既に全部止まっていても成功
- `rate-limited` は、実際に spawn が必要でかつ直近の spawn 試行から 1 秒未満のときだけ返る
  (既に全部 running なら常に成功)
- 全プロセスは `setsid` で新しいプロセスグループに置く。停止は `killpg(SIGTERM)` → 3 秒猶予 →
  `killpg(SIGKILL)`。孫プロセスも道連れにする
- 子の stdout/stderr は `[sidecar:<plugin-id>/<name>]` タグでホストのログへ流す。プラグインには渡さない

### ホストが強制的に停止する契機

| 契機 | 挙動 |
|---|---|
| デーモン停止 | 全プラグインの全サイドカーを停止(`Drop` に頼らず明示的な shutdown 経路で) |
| プラグイン無効化 / 再ロード | そのプラグインの全サイドカーを停止 |
| 承認取消・失効 | 即座に停止し、以後の呼び出しは `permission-denied` |
| `command` / `args` / `port` / `replicas` の変更 | 停止する。次の `ensure-started` で新設定で起動 |
| UI からの停止 / 再起動 | 停止(再起動は停止後にホストが即 spawn) |

## RPC 追加

既存の 3 メソッドと同じ `{"type":"rpc", "id":…, "method":…, "params":…}` 形式。

- `plugins/get-sidecars` `{"plugin": "<id>"}` → 下記の result
- `plugins/set-sidecar-config` `{"plugin": "<id>", "name": "<name>", "config": {…}}` → 同形。
  検証エラーは `rpc-error` で、値は変更されない
- `plugins/set-sidecar-grant` `{"plugin": "<id>", "name": "<name>", "granted": <bool>}` → 同形
- `plugins/sidecar-control` `{"plugin": "<id>", "name": "<name>", "action": "start"|"stop"|"restart"}` → 同形
- `plugins/list` の各要素に `sidecars` を追加

result の形:

```jsonc
{
  "sidecars": [
    {
      "name": "tts",
      "reason": "音声合成エンジンをローカルで動かすため",
      "args": ["--port", "{port}"],   // manifest の既定値
      "port": 50021,                  // manifest の既定値
      "scalable": true,
      "granted": false,
      "staleGrant": false,
      "config": { "command": "/usr/bin/piper", "args": ["--port", "{port}"], "port": 50021, "replicas": 2 },
      "instances": [
        { "index": 0, "port": 50021, "state": "running", "exitCode": null },
        { "index": 1, "port": 50022, "state": "exited",  "exitCode": 1 }
      ]
    }
  ]
}
```

未知の `plugin` / `name` は `rpc-error`。

## UI

Plugins 画面の各プラグインカードに、capability セクションの隣へサイドカーセクションを追加する。
`[[sidecar]]` の無いプラグインには何も表示しない。

- サイドカーごとに `reason`、実行ファイルパス入力(Settings の journal dir と同じネイティブ
  ファイルピッカーを流用)、args、port、`scalable` なら replicas
- 承認トグル。`command` 未入力の間は無効。未承認時は「未承認 — このプラグインはプロセスを
  起動できません」
- 承認時の警告文は HTTP capability より強くする:
  **「承認するとこのプラグインはあなたが指定したプログラムを実行できます。そのプログラムは
  edlr のサンドボックスの外で動きます」**
- 失効時(`staleGrant`)は「要求が変わったため再承認が必要」と警告表示
- インスタンス一覧(index / port / state / 終了コード)と、起動・停止・再起動ボタン

## テスト方針

- **manifest**: `[[sidecar]]` のパース・検証(`name` 重複、不正な `name`、`reason` 欠落、
  範囲外 `port`、`scalable` の既定値)、フィンガープリントの安定性と `command` 非依存性
- **設定ストア**: 永続化、ポート採番、`{port}` 無し + `replicas > 1` の拒否、
  `scalable = false` + `replicas > 1` の拒否、ポート範囲重複の拒否、変更時に停止すること
- **grants**: 既定未承認、サイドカー単位の承認、要求変更での失効、取消で即停止、
  `command` 未設定では承認できないこと
- **`drivers/process` の Supervisor 単体**: 冪等な `ensure-started`、レート制限、
  `stop` の SIGTERM → SIGKILL 昇格、プロセスグループ kill で孫プロセスも死ぬこと
  (`sh -c 'sleep 100 & wait'` を起動して孫の生存を確認)、異常終了後の `exited` + exit-code
- **実 wasm 統合**: 未承認で `permission-denied`、承認後に起動したテスト用 HTTP サーバへ
  `driver-http` が通る、暗黙許可が採番ポートのみに効く(隣のポートは拒否)、
  承認取消後に `driver-http` が `permission-denied` に戻る、デーモン shutdown で子が残らない
- **RPC / UI**: 設定・承認・start/stop/restart、インスタンス表示、失効表示

## スコープ外

- 自動再起動・ヘルスチェック・readiness 待ち(プラグインのリトライに委ねる)
- ロードバランス(プラグインが `status()` の一覧から自分で選ぶ)
- stdio 経由の通信、サイドカーの stdout をプラグインへ渡すこと
- 環境変数の注入、cgroup / rlimit によるリソース制限
- サイドカーバイナリの配布・署名・バージョン管理
- チャネルドライバ(別フェーズ)
