// tutorial-jump-log は docs/plugin-tutorial-tinygo.md で育てるプラグインの
// 完成形。
//
// このファイルはホスト境界のアダプタに徹する。設定を読み、bus へ publish し、
// driver-http を呼び、結果をログへ整形するだけで、判断は jumplog パッケージが
// 持つ(main は //go:wasmimport を含むためテストが書けない)。
package main

import (
	"fmt"
	"strings"

	"go.bytecodealliance.org/cm"

	bus "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/bus"
	bustypes "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/bus-types"
	driverhttp "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/driver-http"
	hostlog "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/host-log"
	hostsettings "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/host-settings"
	plugin "github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/gen/edlr/plugin/plugin"
	"github.com/himanoa/edlr/examples/plugins/tutorial-jump-log-go/jumplog"
)

const (
	// tracker は連携するドライバの id(manifest の `[[bus]]` と一致させる)。
	tracker = "tutorial-tracker-go"
	// edsm は問い合わせ先。manifest の `[[capabilities]]` で
	// https://www.edsm.net を要求し、ユーザーが承認したときだけ通る。
	edsm = "https://www.edsm.net/api-v1/system"
	// queueCap は未処理のジャンプを溜めておく上限。
	queueCap = 50
)

var pending *jumplog.Queue

// init で export を登録する。ホストはここで登録した関数を直接呼ぶ。
func init() {
	plugin.Exports.Init = onInit
	plugin.Exports.OnEvent = onEvent
	plugin.Exports.OnMessage = onMessage
	plugin.Exports.OnSchedule = onSchedule
	plugin.Exports.OnStop = onStop
}

// main は TinyGo がコンポーネントをビルドするために要る。エントリポイント
// としては使われない。
func main() {}

func onInit() {
	pending = jumplog.NewQueue(queueCap)
	logf(hostlog.LevelInfo, "tutorial-jump-log started")
}

func onEvent(ev plugin.Event) {
	// manifest の `events` で絞ってはいるが、購読を増やしたときに壊れない
	// よう、ここでも名前を確かめる。
	if name := ev.Name.Some(); name == nil || *name != "FSDJump" {
		return
	}

	settings := jumplog.ParseSettings(hostsettings.GetAll())
	if !settings.Enabled {
		return
	}

	jump, ok := jumplog.ParseJump(ev.PayloadJSON)
	if !ok {
		logf(hostlog.LevelWarn, "could not read StarSystem from the payload")
		return
	}
	if jump.Distance < settings.MinDistance {
		// debug ではなく info。デーモンのログレベルは INFO 固定で、debug は
		// どこにも出ない(チュートリアル 2 章の「確認する」参照)。
		logf(hostlog.LevelInfo, "skipping %s (%.2f ly < %.2f ly)",
			jump.System, jump.Distance, settings.MinDistance)
		return
	}

	logf(hostlog.LevelInfo, "jumped to %s (%.2f ly)", jump.System, jump.Distance)

	// ドライバへ渡す。未承認のうちは permission-denied になるが、プラグイン
	// 自体は動き続けてよいので、ログに出して先へ進む。
	published := bus.Publish(tracker, "visit", cm.ToList([]byte(jump.System)))
	if err := published.Err(); err != nil {
		logf(hostlog.LevelWarn, "publish failed: %s", busErrorMessage(err))
	}

	pending.Push(jump)
}

// onMessage はドライバが last-system へ流し直した値を受け取る
// (manifest の `[[bus]]` の subscribe に書いたトピックだけが届く)。
func onMessage(driver string, topic string, payload cm.List[uint8]) {
	logf(hostlog.LevelInfo, "%s/%s = %s", driver, topic, string(payload.Slice()))
}

func onSchedule(name string) {
	if name != "flush" {
		logf(hostlog.LevelWarn, "unknown schedule: %s", name)
		return
	}

	jump, ok := pending.Pop()
	if !ok {
		return
	}

	// 呼び出し期限は 2 秒、driver-http のタイムアウトは 1.5 秒。
	// 1 回の呼び出しで HTTP を叩けるのは実質 1 回だけ。
	lookup(jump)

	if n := pending.Len(); n > 0 {
		logf(hostlog.LevelInfo, "%d jump(s) still queued", n)
	}

	// retain されている最新値は、配信を待たずいつでも読める。
	result := bus.Get(tracker, "last-system")
	if err := result.Err(); err != nil {
		logf(hostlog.LevelWarn, "get failed: %s", busErrorMessage(err))
	} else if v := result.OK().Some(); v != nil {
		logf(hostlog.LevelInfo, "tracker says: %s", string(v.Slice()))
	}
}

// onStop はデーモンの graceful shutdown で一度だけ呼ばれる。猶予は 5 秒しか
// なく、ここで HTTP を叩くと間に合わないことがあるので、残りはログへ落とす
// だけにする。
func onStop() {
	if pending.Len() == 0 {
		return
	}
	logf(hostlog.LevelInfo, "stopping with %d unflushed: %s",
		pending.Len(), pending.Summary())
}

// lookup は EDSM に星系を問い合わせて、結果をログへ落とす。
func lookup(jump jumplog.Jump) {
	result := driverhttp.Send(driverhttp.Request{
		Method:  "GET",
		URL:     jumplog.EDSMURL(edsm, jump.System),
		Headers: cm.ToList([][2]string{{"accept", "application/json"}}),
		Body:    cm.None[cm.List[uint8]](),
	})

	if err := result.Err(); err != nil {
		logf(hostlog.LevelWarn, "%s: lookup failed: %s",
			jump.System, driverErrorMessage(err))
		return
	}

	resp := result.OK()
	if resp.Status != 200 {
		logf(hostlog.LevelWarn, "%s: EDSM returned HTTP %d", jump.System, resp.Status)
		return
	}

	body := strings.TrimSpace(string(resp.Body.Slice()))
	// 未知の星系では EDSM は空配列 `[]` を返す。
	if body == "[]" {
		logf(hostlog.LevelInfo, "%s: not known to EDSM", jump.System)
		return
	}
	logf(hostlog.LevelInfo, "%s: EDSM says %s", jump.System, body)
}

// driverErrorMessage は variant の中身(理由文字列)を取り出す。
func driverErrorMessage(err *driverhttp.DriverError) string {
	for _, m := range []*string{err.PermissionDenied(), err.InvalidRequest(), err.Transport()} {
		if m != nil {
			return *m
		}
	}
	return "unknown driver error"
}

func busErrorMessage(err *bustypes.BusError) string {
	for _, m := range []*string{
		err.PermissionDenied(), err.UnknownDriver(), err.UnknownTopic(),
		err.DriverUnavailable(), err.QueueFull(), err.TooLarge(),
	} {
		if m != nil {
			return *m
		}
	}
	return "unknown bus error"
}

func logf(level hostlog.Level, format string, args ...any) {
	hostlog.Log(level, fmt.Sprintf(format, args...))
}
