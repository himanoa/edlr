# CLI とデーモンの挙動

## 起動

    cargo run -p edlr-core --bin edlr -- --journal-dir <JournalディレクトリのPATH>

`--journal-dir` 省略時は以下の順で解決する: CLI 引数 → `config.json` の `journalDir` → Proton の既定パス自動検出 → フォールバック(`$XDG_DATA_HOME/edlr/journal`, `XDG_DATA_HOME` 未設定なら `~/.local/share/edlr/journal`)を作成して使用。イベントは 1 行 1 JSON で stdout に流れる。

## CLI フラグ一覧

| フラグ | 既定 | 説明 |
| --- | --- | --- |
| `--journal-dir` | config.json → Proton 既定パス → フォールバック(`$XDG_DATA_HOME/edlr/journal` など)を自動作成 | Journal ディレクトリ |
| `--poll-interval-ms` | `1000` | ポーリング間隔(ミリ秒) |
| `--listen` | `127.0.0.1:8137` | HTTP/WebSocket の listen アドレス |
| `--ui-dir` | (未指定なら配信しない) | UI 静的ファイルのディレクトリ |
| `--plugins-dir` | `$XDG_CONFIG_HOME/edlr/plugins`(未設定なら `~/.config/edlr/plugins`) | プラグインディレクトリ |
| `--drivers-dir` | `$XDG_CONFIG_HOME/edlr/drivers`(未設定なら `~/.config/edlr/drivers`) | ドライバディレクトリ |
| `--settings-dir` | `$XDG_CONFIG_HOME/edlr/settings`(未設定なら `~/.config/edlr/settings`) | プラグイン設定の保存先 |
| `--grants-dir` | `$XDG_CONFIG_HOME/edlr/grants`(未設定なら `~/.config/edlr/grants`) | capability 承認の保存先 |
| `--state-dir` | `$XDG_STATE_HOME/edlr`(未設定なら `~/.local/state/edlr`) | Journal 読み取り位置の保存先(下記参照) |

`--plugins-dir` / `--drivers-dir` / `--settings-dir` / `--grants-dir` は、
指すディレクトリが存在しなくてもエラーにはならない(それぞれプラグイン 0 件・
ドライバ 0 件・全プラグイン未承認として起動する)。

## Journal 読み取り位置の永続化

`edlr` は Journal の読み取り位置を `<state-dir>/journal-position.json` に
(監視している Journal ディレクトリをキーにして)保存し、デーモンを再起動した
ときはその位置から読み取りを再開する。これが無かった旧バージョンでは、
再起動のたびに現行 Journal ファイルを先頭から読み直し、その日のイベントを
丸ごと再配信していた。

保存は行の配信直後に行われ、位置が前回保存時から変わっていなければ書き込まない
(ゲームを起動していない間、毎ポーリングで同じ内容を書き直さないため)。

書き込みに失敗しても(`state-dir` に書けない、ディスクフルなど)デーモンは
止まらない。**警告ログは 1 度しか出ない**が、諦めるわけではなく、位置が動く
たびに保存を試み続ける。途中で書けるようになれば黙って成功に戻る(その旨の
ログは出ない)。デーモンが落ちるまで一度も書けなかった場合は、次回起動時は
最後に保存できた位置(まったく書けていなければ最新ファイルの先頭)から読む。

**位置の永続化はルーター層までの保証**であることに注意。ルーターから先、
プラグインごとの作業キュー(64 件、`PLUGIN_WORK_QUEUE_CAPACITY`。journal
イベントとバス配信が同じ枠を共有する)が溢れた場合、そのプラグインぶんの
イベントはホスト側で捨てられ、**位置は
既に進んでいるので二度と届かない**。位置の永続化が無かった頃は、デーモンを
再起動すれば同じ Journal を先頭から読み直すため取りこぼしを偶然拾い直せた
が、その安全網は無くなった(プラグインが `driver-http` などの同期呼び出しで
長く止まると顕在化する。`examples/plugins/inara-uploader/README.md` の
「送信中はイベントを取りこぼしうる」を参照)。

**同じ Journal ディレクトリを複数の `edlr` デーモンで同時に監視する構成は
サポートしない**。読み取り位置ファイルへの書き込みは 1 プロセス内でしか
直列化されないため、複数プロセスが同じキーへ保存すると競合し、位置が
壊れたり巻き戻ったりし得る。

## `replay` フラグ

デーモンが動き出す前に、監視対象の Journal ファイルへ**既に書かれていた**
イベントには、配信されるイベントの `replay` フィールドが `true` になる
(初回起動でファイルを先頭から読む場合も、保存済み位置から再開してその位置
より前の内容を読む場合も同様)。デーモン起動後に新しく書かれたイベントは
常に `replay: false`。WebSocket で流れる `journal` 種別のイベントにも
同じ `replay` が乗る(`status` 種別は「今の状態のスナップショット」なので
`replay` の概念自体が無く、常に `false`)。

用途の目安:

- 通知・音を鳴らす系のプラグインは `replay` のイベントを無視するのが自然
  (デーモン起動時にゲーム内の出来事が一斉に鳴り直すのを避けるため)
- 外部サービスへのアップロード・集計系のプラグインは、位置の永続化により
  再起動をまたいだ重複配信が起きなくなったので、`replay` のイベントも
  安全に処理してよい(取りこぼしを避けたいなら処理すべき)
