# サイドカーの起動完了をドライバへ通知する(sidecar-ready)

## 背景

coeiroink ドライバは `init()` でワーカーの `/speakers` を取得して `speakers`
retain トピックへ載せるが、init 時点ではワーカーがまだ listen していないのが
普通で、publish は失敗を許容している(「最初の読み上げ依頼で取り直される」)。

その結果、デーモン起動から最初の読み上げまでの間、`options-from =
{ driver = "coeiroink", topic = "speakers" }` を指す select 設定は
「候補を取得できません(トピック speakers が未着です)」になる。retain は
メモリ上なので、この空白期間はデーモンを再起動するたびに再発する。

根本原因は「ワーカーが listen し始めた瞬間を知る手段がドライバに無い」こと。
ドライバの world は `init` / `on-message` しか export しておらず(意図的:
ドライバは journal を受けない)、on-message はプラグインの publish でしか
呼ばれない。

## 方針

**ホストが spawn したインスタンスの port を監視し、初めて TCP 接続できた
時点でドライバの `on-message` へ合成メッセージを届ける。**

- ワーカー側は無変更(coeiroink ワーカーは listen 開始前に話者カタログを
  読み込み済みなので、port が繋がった時点で `/speakers` は応答できる)
- WIT の変更も不要(既存の `on-message` export に相乗りする)
- ワーカーがデーモンへコールバックする案は、呼び出し元ワーカーの認証・
  識別(トークンや port の対応付け)が新たに要るため採らない

## edlr 側の変更

### port 監視(driver-process ホスト)

インスタンスを spawn したら、インスタンスごとにバックグラウンドの監視
タスクを立てる:

- 割当 port へ TCP 接続を約 200ms 間隔でポーリングする
- 初めて接続できたら ready を 1 回通知して監視を終える
- 接続できる前にプロセスが死んだら、通知せず監視を終える
- タイムアウトは設けない。COEIROINK エンジンのロードは分単位になりうる。
  プロセス死亡が実質の打ち切り条件
- respawn したら新しい監視を立て直す(= 再起動のたびに再通知される)

### 配送(driver runner)

通知はドライバ専用スレッドの作業キューに入り、次の形で届く:

```
on-message(from = "host", topic = "sidecar-ready",
           payload = {"name": "<sidecar名>", "index": <n>, "port": <p>})
```

`from = "host"` の予約: `host` はプラグイン id の字種 `[a-z0-9-]+` として
合法なので、なりすまし余地を塞ぐため manifest 検証で `id = "host"` を
拒否する。

対象はドライバのサイドカーのみ。プラグインのサイドカーへの展開は必要に
なってから。

## coeiroink ドライバ側の変更

`on_message` に分岐を足す: `from == "host" && topic == "sidecar-ready"` で
payload の `index == 0` なら `publish_speakers()` を呼ぶ。

あわせて `SPEAKERS_PUBLISHED` の一発フラグは「ready 通知を受けたら
リセットして再 publish」に変える。ワーカー再起動で話者構成が変わった場合
(ボイスの追加・削除)も retain が追従する。既存の「speak 受信時に取り直す」
経路は保険としてそのまま残す。

## テスト

edlr:

- `ProcessDriver`: 「遅れて listen するダミープロセス」で ready 通知が
  来る/先に死んだら来ない/respawn で再通知される
- driver runner: sidecar-ready がドライバの `on-message` に
  `from = "host"` で配送される
- manifest: `id = "host"` のプラグインが弾かれる

coeiroink:

- `on_message("host", "sidecar-ready", ...)` の index 0 で publish が走る、
  index != 0 では走らない
- ready 再通知で speakers が再 publish される

## 効果

デーモン起動直後、ワーカーが listen し始めた瞬間に `speakers` が retain
され、一度も読み上げていなくても設定画面のプルダウンに候補が出る。
デーモン再起動後も同様。

## やらないこと

- プラグインのサイドカーへの ready 通知(必要になってから)
- ready の定義の高度化(HTTP ヘルスチェック等)。TCP 接続可能で足りる
  ことを coeiroink ワーカーの起動順(カタログ読み込み → listen)で確認済み
- ワーカー → デーモンのコールバック経路
