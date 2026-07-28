// inara-uploader は Journal イベントを INARA (https://inara.cz) の
// API v1 へアップロードする edlr プラグイン。
//
// このファイルはホスト境界のアダプタに徹する。設定を読んで uploader へ渡し、
// driver-http を呼び、返ってきた Outcome をログへ整形するだけで、判断は
// 持たない(main は //go:wasmimport を含むためテストが書けない)。
//
// 設計上の前提:
//   - プラグインはイベント到着時にしか動けない(ホストにタイマーが無い)。
//     そのため送信は「イベントを受け取ったついでに」行う。最後のイベントから
//     ゲーム終了までの間に溜まったぶんは、Journal の `Shutdown` でまとめて送る。
//   - edlr は Journal の読み取り位置を永続化しており、デーモン再起動をまたいで
//     続きから配信する。デーモンが動き出す前に既に書かれていたイベントには
//     `event.replay` が立つが、重複配信は起きないので既定ではこれも送る。
//
// 詳細は README.md を参照。
package main

import (
	"fmt"
	"time"

	"go.bytecodealliance.org/cm"

	driverhttp "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/driver-http"
	hostlog "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-log"
	hostsettings "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-settings"
	plugin "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/plugin"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/uploader"
)

// inaraEndpoint は INARA API v1 のエンドポイント。manifest の
// `[[capabilities]]` で `https://inara.cz` を要求し、ユーザーが承認した
// 場合にだけ `driver-http.send` が通る。
const inaraEndpoint = "https://inara.cz/inapi/v1/"

var up *uploader.Uploader

func init() {
	plugin.Exports.Init = onInit
	plugin.Exports.OnEvent = onEvent
}

// main は TinyGo が component をビルドするために必要。エントリポイントとしては
// 使われない(ホストは `init` / `on-event` の export を直接呼ぶ)。
func main() {}

func onInit() {
	up = uploader.New(func() time.Time { return time.Now().UTC() }, httpSender{})
	logf(hostlog.LevelInfo, "inara-uploader initialized")
}

func onEvent(ev plugin.Event) {
	cfg := settings.Parse(hostsettings.GetAll())
	report(cfg, up.Handle(cfg, uploader.Event{
		Kind:      ev.Kind,
		Name:      option(ev.Name),
		Timestamp: option(ev.Timestamp),
		Payload:   ev.PayloadJSON,
		Replay:    ev.Replay,
	}))
}

// option は WIT の option<string> を素の string にする。
func option(o cm.Option[string]) string {
	if v := o.Some(); v != nil {
		return *v
	}
	return ""
}

// httpSender は driver-http による送信。
//
// ホスト側で 1.5 秒のタイムアウトが掛かっており、リダイレクトも追わない。
// タイムアウトやネットワークエラーはここで error になり、uploader が
// キューを保持して再試行する。
type httpSender struct{}

func (httpSender) Send(body []byte) (int, []byte, error) {
	result := driverhttp.Send(driverhttp.Request{
		Method: "POST",
		URL:    inaraEndpoint,
		Headers: cm.ToList([][2]string{
			{"content-type", "application/json"},
			{"accept", "application/json"},
		}),
		Body: cm.Some(cm.ToList(body)),
	})

	if err := result.Err(); err != nil {
		return 0, nil, fmt.Errorf("%s: %s", err.String(), driverErrorMessage(err))
	}

	resp := result.OK()
	return int(resp.Status), resp.Body.Slice(), nil
}

// driverErrorMessage は variant の中身(理由文字列)を取り出す。
func driverErrorMessage(err *driverhttp.DriverError) string {
	if m := err.PermissionDenied(); m != nil {
		return *m
	}
	if m := err.InvalidRequest(); m != nil {
		return *m
	}
	if m := err.Transport(); m != nil {
		return *m
	}
	return "unknown driver error"
}

// report は Outcome をログへ落とす。判断はせず、起きたことをそのまま出す。
func report(cfg settings.Settings, out uploader.Outcome) {
	// 開発モード中は、送信を試みたこと自体を残す。結果のログ(uploaded /
	// rejected / Err)だけだと「そもそも送信が走ったのか」が追えないため。
	if cfg.IsBeingDeveloped && out.Attempted > 0 {
		logf(hostlog.LevelInfo,
			"developer mode: sending %d event(s) to inara", out.Attempted)
	}
	if out.Skipped > 0 {
		logf(hostlog.LevelInfo,
			"skipped %d event(s) from the replayed backlog (set uploadHistorical to send them)",
			out.Skipped)
	}
	if out.Dropped > 0 {
		logf(hostlog.LevelWarn,
			"dropped %d queued event(s); the queue keeps the most recent %d",
			out.Dropped, uploader.MaxQueued)
	}
	if out.Held != "" {
		logf(hostlog.LevelWarn, "holding %d event(s): %s", out.Pending, out.Held)
	}
	if out.DryRun != nil {
		logf(hostlog.LevelInfo, "dry run: would upload %d event(s): %s", out.Sent, string(out.DryRun))
	}
	for _, rejected := range out.Rejected {
		logf(hostlog.LevelWarn, "inara rejected event %d: %d %s",
			rejected.Index, rejected.Status, rejected.StatusText)
	}
	if out.Warning != "" {
		// ヘッダの eventStatus が 200 以外の 2xx(202/204)。形式上は成功
		// なので Fatal ではないが、内容は把握できるよう WARN で残す。
		logf(hostlog.LevelWarn, "inara returned a warning for the batch: %s", out.Warning)
	}
	for _, warned := range out.Warned {
		logf(hostlog.LevelWarn, "inara returned a warning for event %d: %d %s",
			warned.Index, warned.Status, warned.StatusText)
	}
	if out.Err != nil {
		level := hostlog.LevelWarn
		if out.Fatal {
			level = hostlog.LevelError
		}
		logf(level, "%v", out.Err)
	}
	if out.Sent > 0 && out.DryRun == nil {
		logf(hostlog.LevelInfo, "uploaded %d event(s) to inara", out.Sent)
	}
}

func logf(level hostlog.Level, format string, args ...any) {
	hostlog.Log(level, fmt.Sprintf(format, args...))
}
