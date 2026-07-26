# edlr ドライバ capability モデル + HTTP ドライバ 設計書

2026-07-26 承認。spec.md の未決定事項「ドライバの capability モデルの詳細(付与・宣言・検証方法)」の確定と、
同梱 HTTP ドライバの実装。チャネルドライバは通信セマンティクスが別の未決定事項のため次フェーズ。

## 中核の方針

- **検証は呼び出し時の一点**。ホスト許可リストは URL が実行時の値なので呼び出し時チェックが必須であり、
  リンカでの import 着脱は二重の仕組みになるため採らない。結果として **承認は即座に反映され、
  プラグインの再起動が不要**(grants は設定値と同じく共有バッファで生きたまま更新される)
- ドライバ関数は `result<_, driver-error>` を返す。未承認・許可外は型付きエラーであり trap ではないので、
  プラグインは握って続行できる
- 許可リストは**プラグインごとのデータ**。ドライバ自身は固有の許可リストを持たない

### 責務の分担

| 置き場所 | 内容 | 誰が決めるか |
|---|---|---|
| プラグインの `manifest.toml` | どこに繋ぎたいかの**要求** | プラグイン作者 |
| `<grants-dir>/<plugin-id>.json` | どれを許可したかの**承認結果** | ユーザー(UI) |
| ドライバ(実行時) | 呼び出しごとの照合 | — |
| `drivers/http` crate | HTTP を実行する能力そのもの | — |

### 詐称不可の性質

プラグインは自分の ID も許可リストも引数で渡さない。`HostCtx`(wasm インスタンスごとに固有)が
`plugin_id` と設定 JSON を保持している既存の仕組みに grants を載せ、ドライバのホスト実装は
自分の `HostCtx` からのみ許可リストを読む。プラグイン側からは参照も改変もできず、
他プラグインの許可を騙ることもできない。

## マニフェストでの宣言

```toml
[[capabilities]]
kind = "http"
hosts = ["https://api.example.com", "https://tts.local:8080"]
reason = "音声合成 API に読み上げテキストを送るため"
```

- `kind` は既知の種類のみ(現在は `http`)。未知の kind はマニフェスト検証エラー(そのプラグインのみロードしない)
- `hosts` は スキーム + ホスト + 任意でポート。パスは見ない。スキームは `http` / `https` のみ
- `reason` は必須。承認画面に表示する
- `capabilities` 省略時は要求なし(従来どおり log・settings のみ)

## 付与(grants)

- 保存先: `<grants-dir>/<plugin-id>.json`(既定 `$XDG_CONFIG_HOME/edlr/grants`、`--grants-dir` で上書き)
- 形: どの要求を承認したかと、承認時点の**要求ハッシュ**
- **既定は未承認**。未承認でもプラグインは通常どおり起動する(capability なしで動く)。
  未承認の driver を呼ぶと `permission-denied` が返る
- マニフェストの要求が変わったら(ホスト追加など)ハッシュ不一致で承認は**自動失効**し、UI が再承認を促す
- 承認・取消は即座に共有バッファへ反映され、次の driver 呼び出しから有効

## WIT 追加

```wit
interface driver-http {
  record request {
    method: string,
    url: string,
    headers: list<tuple<string, string>>,
    body: option<list<u8>>,
  }
  record response {
    status: u16,
    headers: list<tuple<string, string>>,
    body: list<u8>,
  }
  variant driver-error {
    permission-denied(string),
    invalid-request(string),
    transport(string),
  }
  send: func(req: request) -> result<response, driver-error>;
}
```

`world plugin` に `import driver-http;` を追加する。ホストは常にこのインターフェースを提供し、
許可判定は `send` の実装内で行う。

## HTTP ドライバ(`drivers/http`)

- reqwest のブロッキング API を使用(wasm 呼び出しは既に専用スレッド上のため整合的)
- **リダイレクトは追従しない**(許可外ホストへ飛ばされるのを防ぐ)。3xx はそのままレスポンスとして返す
- タイムアウト既定 10 秒、レスポンスボディ上限既定 8 MiB(超過は `transport` エラー)
- 許可判定: スキーム + ホスト + ポートの完全一致。ポート省略時はスキーム既定ポート(http=80 / https=443)に正規化。
  ホストの大文字小文字は無視。サブドメインのワイルドカードは**なし**(`api.example.com` の許可は `x.api.example.com` を許可しない)
- URL パース失敗・非 http(s) スキーム・許可外は呼び出し前に弾く

## RPC 追加

```jsonc
{ "type": "rpc", "id": <number>, "method": "plugins/get-capabilities", "params": { "plugin": "<id>" } }
{ "type": "rpc", "id": <number>, "method": "plugins/set-capabilities", "params": { "plugin": "<id>", "granted": <bool> } }
```

- `get-capabilities` の result: `{ requests: [{ kind, hosts, reason }], granted: <bool>, staleGrant: <bool> }`
  (`staleGrant` は承認済みだが要求ハッシュが変わって失効している状態)
- `set-capabilities` は承認/取消をまとめて行う(要求単位ではなくプラグイン単位。要求が複数あっても
  「このプラグインの要求一式を許可するか」で扱う)。result は `get-capabilities` と同形
- `plugins/list` の各要素に `capabilities: { requests, granted, staleGrant }` を追加する

## UI

Plugins 画面の各プラグインカードに capability セクションを追加:

- 要求ごとに `kind`・ホスト一覧・`reason` を表示
- 承認トグル(プラグイン単位)。未承認は「未承認 — このプラグインは外部通信できません」と明示
- 失効時(`staleGrant`)は「要求が変わったため再承認が必要」と警告表示
- 要求が無いプラグインには何も表示しない

## テスト方針

- マニフェスト: capability のパース・検証(未知 kind、不正スキーム、空 hosts、reason 欠落)、要求ハッシュの安定性
- 許可判定の単体テスト: スキーム違い・ポート違い(既定ポート正規化含む)・大文字小文字・サブドメイン・パス無視
- grants ストア: 永続化、既定未承認、要求変更での失効、取消
- 実 wasm 統合: 未承認プラグインの `send` が `permission-denied`、承認後にローカルのテスト用 HTTP サーバへ通る、
  許可外ホストは承認後も `permission-denied`、リダイレクトを追従しない
- RPC・UI: 承認/取消/失効表示

## スコープ外

- チャネルドライバ(通信セマンティクスが別の未決定事項)
- 要求単位での部分承認(プラグイン単位でまとめて承認)
- デーモン全体での HTTP ドライバ無効化・グローバル拒否ホスト
- 認証情報の保管(プラグインが自前でヘッダに載せる想定)
- アーカイブ配布形式・署名
