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
	"bytes"
	"fmt"
	"time"

	"go.bytecodealliance.org/cm"

	"github.com/himanoa/edlr/sdk/go/edlrplugin"
	driverfs "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/driver-fs"
	driverhttp "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/driver-http"
	hostlog "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/host-log"
	hostsettings "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/host-settings"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/uploader"
)

// inaraEndpoint は INARA API v1 のエンドポイント。manifest の
// `[[capabilities]]` で `https://inara.cz` を要求し、ユーザーが承認した
// 場合にだけ `driver-http.send` が通る。
const inaraEndpoint = "https://inara.cz/inapi/v1/"

// stateRoot は manifest の [[filesystem]] name、stateFile はその中のファイル名。
// Journal から学習した状態(コマンダー名・gameversion)の保存先。edlr は
// 読み取り位置を永続化していて LoadGame は再配信されないため、ここへ保存
// しないとセッション途中のデーモン再起動で Live ゲートが閉じたままになる。
const (
	stateRoot = "state"
	stateFile = "state.json"
)

// journalRoot は manifest の [[filesystem]] name="journal"(手動同期が
// Journal を読み直すルート)。actionResync はダッシュボードウィジェットが
// `edlr.action("resync")` で送るアクション名。
const (
	journalRoot  = "journal"
	actionResync = "resync"
)

var up *uploader.Uploader
var resync *uploader.Resync

// lastSavedState は最後に保存した(または保存を試みた)状態のスナップショット。
// イベントのたびに書き込まないための比較用。
var lastSavedState []byte

func init() {
	edlrplugin.Register(edlrplugin.Hooks{
		Init:       onInit,
		OnEvent:    onEvent,
		OnMessage:  onMessage,
		OnSchedule: onSchedule,
		OnStop:     onStop,
	})
}

// main は TinyGo が component をビルドするために必要。エントリポイントとしては
// 使われない(ホストは `init` / `on-event` の export を直接呼ぶ)。
func main() {}

func onInit() {
	up = uploader.New(func() time.Time { return time.Now().UTC() }, httpSender{})
	resync = uploader.NewResync(journalFS{}, asyncSender{})
	loadState()
	logf(hostlog.LevelInfo, "inara-uploader initialized")
}

// onMessage はダッシュボードウィジェットのアクションを受ける
// (driver は core の予約 id "dashboard"。docs/drivers.md 参照)。
func onMessage(driver, topic string, _ []byte) {
	if driver != "dashboard" || topic != actionResync {
		logf(hostlog.LevelWarn, "unknown message: %s/%s", driver, topic)
		return
	}
	cfg := settings.Parse(hostsettings.GetAll())
	reportResync(resync.Start(cfg))
}

func onEvent(ev edlrplugin.Event) {
	cfg := settings.Parse(hostsettings.GetAll())
	report(cfg, up.Handle(cfg, uploader.Event{
		Kind:      ev.Kind,
		Name:      option(ev.Name),
		Timestamp: option(ev.Timestamp),
		Payload:   ev.PayloadJSON,
		Replay:    ev.Replay,
	}))
	saveState()
}

// onSchedule は manifest の `[[schedule]]` から呼ばれる。このプラグインが
// 宣言しているスケジュールは "flush" 1 つだけなので、name はそれ以外を
// 無視するためだけに見る(将来 2 つ目のスケジュールを足したら、ここで
// 分岐する必要がある)。
//
// minIntervalSeconds を尊重した定期フラッシュで、直近のイベントで送り
// 切れなかった端数を拾い上げる。
func onSchedule(name string) {
	if name != "flush" {
		logf(hostlog.LevelWarn, "unknown schedule name: %s", name)
		return
	}
	cfg := settings.Parse(hostsettings.GetAll())
	report(cfg, up.HandleSchedule(cfg))
}

// onStop はデーモンの graceful shutdown 時に一度だけ呼ばれる。次の
// イベントはもう来ないので、間隔を無視して最後の機会としてフラッシュする。
func onStop() {
	cfg := settings.Parse(hostsettings.GetAll())
	report(cfg, up.HandleStop(cfg))
	saveState()
}

// loadState は保存済みの学習状態を driver-fs から読み戻す。
//
// 読めないのは異常ではない(ルート未承認・ディレクトリ未設定・初回起動)ので
// 情報ログ 1 行にとどめ、空から始める(次の LoadGame まで送信しない従来挙動)。
func loadState() {
	result := driverfs.Read(stateRoot, stateFile)
	if err := result.Err(); err != nil {
		logf(hostlog.LevelInfo,
			"保存済みの状態を読めませんでした。次の LoadGame まで学習し直します: %s",
			err.String())
		return
	}
	raw := result.OK().Slice()
	up.RestoreState(raw)
	lastSavedState = raw
}

// saveState は学習状態が変わっていれば driver-fs へ書く。
//
// 書けなかった場合も lastSavedState を進める: 書けない状況(未承認など)で
// イベントのたびに警告を繰り返さず、状態が変わったときだけ再試行する。
func saveState() {
	raw, err := up.StateSnapshot()
	if err != nil {
		logf(hostlog.LevelWarn, "状態を JSON にできませんでした: %v", err)
		return
	}
	if bytes.Equal(raw, lastSavedState) {
		return
	}
	lastSavedState = raw
	result := driverfs.Write(stateRoot, stateFile, cm.ToList(raw))
	if e := result.Err(); e != nil {
		logf(hostlog.LevelWarn,
			"状態を保存できませんでした(デーモン再起動でコマンダー名と gameversion を再学習します): %s",
			e.String())
	}
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

// journalFS は uploader.FS の driver-fs 実装(手動同期の Journal 読み直し)。
type journalFS struct{}

func (journalFS) List() ([]uploader.FileInfo, error) {
	result := driverfs.List(journalRoot, "")
	if err := result.Err(); err != nil {
		return nil, fmt.Errorf("%s", err.String())
	}
	entries := result.OK().Slice()
	infos := make([]uploader.FileInfo, 0, len(entries))
	for _, e := range entries {
		infos = append(infos, uploader.FileInfo{Path: e.Path, Size: e.Size})
	}
	return infos, nil
}

func (journalFS) ReadRange(path string, offset uint64, n uint32) ([]byte, error) {
	result := driverfs.ReadRange(journalRoot, path, offset, n)
	if err := result.Err(); err != nil {
		return nil, fmt.Errorf("%s", err.String())
	}
	return result.OK().Slice(), nil
}

// asyncSender は uploader.AsyncSender の SubmitHTTP 実装。完了時に
// resync.OnSendResult を呼んで数珠つなぎを続ける。
type asyncSender struct{}

func (asyncSender) Submit(body []byte) error {
	req := edlrplugin.Request{
		Method: "POST",
		URL:    inaraEndpoint,
		Headers: cm.ToList([][2]string{
			{"content-type", "application/json"},
			{"accept", "application/json"},
		}),
		Body: cm.Some(cm.ToList(body)),
	}
	_, err := edlrplugin.SubmitHTTP(req, nil, func(resp *edlrplugin.Response, err error) {
		cfg := settings.Parse(hostsettings.GetAll())
		if err != nil {
			reportResync(resync.OnSendResult(0, nil, err, cfg))
			return
		}
		reportResync(resync.OnSendResult(int(resp.Status), resp.Body, nil, cfg))
	})
	return err
}

// reportResync は ResyncOutcome をログへ落とす。判断はせず整形だけ。
func reportResync(out uploader.ResyncOutcome) {
	if out.Refused != "" {
		logf(hostlog.LevelWarn, "resync: %s", out.Refused)
		return
	}
	for _, rejected := range out.Rejected {
		logf(hostlog.LevelWarn, "resync: inara rejected event %d: %d %s",
			rejected.Index, rejected.Status, rejected.StatusText)
	}
	if out.Warning != "" {
		logf(hostlog.LevelWarn, "resync: inara returned a warning for the batch: %s", out.Warning)
	}
	for _, warned := range out.Warned {
		logf(hostlog.LevelWarn, "resync: inara returned a warning for event %d: %d %s",
			warned.Index, warned.Status, warned.StatusText)
	}
	if out.Err != nil {
		logf(hostlog.LevelError, "resync: %v", out.Err)
		return
	}
	if out.Done {
		logf(hostlog.LevelInfo, "resync: 完了(%d 件送信)", out.Total)
		return
	}
	if out.Submitted > 0 {
		logf(hostlog.LevelInfo, "resync: %d 件送信中(%d/%d bytes)",
			out.Submitted, out.OffsetBytes, out.SizeBytes)
	}
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
