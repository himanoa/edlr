// Package edlrplugin は edlr プラグインを Go で書くための SDK。
// WIT バインディング(gen/)と WIT 本体(wit/)を同梱しているので、
// プラグイン作者は core/wit の cp も wit-bindgen-go の実行も不要。
// 使い方はリポジトリの docs/sdk.md を参照。
package edlrplugin

import (
	"fmt"

	"go.bytecodealliance.org/cm"

	driverhttp "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/driver-http"
	"github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/plugin"
)

// Event / Request は gen の型をそのまま使う(再エクスポート)。
type (
	Event   = plugin.Event
	Request = driverhttp.Request
)

// Hooks は export の実装。未設定(nil)のフックは no-op になる。
// OnJobComplete には SubmitHTTP を経由しない job の完了だけが届く。
type Hooks struct {
	Init          func()
	OnEvent       func(Event)
	OnMessage     func(driver, topic string, payload []byte)
	OnSchedule    func(name string)
	OnJobComplete func(jobID uint64, resultJSON string)
	OnStop        func()
}

// Register は Hooks を wasm の export として配線する。init() から呼ぶこと。
func Register(hooks Hooks) {
	plugin.Exports.Init = func() {
		if hooks.Init != nil {
			hooks.Init()
		}
	}
	plugin.Exports.OnEvent = func(ev Event) {
		if hooks.OnEvent != nil {
			hooks.OnEvent(ev)
		}
	}
	plugin.Exports.OnMessage = func(driver, topic string, payload cm.List[uint8]) {
		if hooks.OnMessage != nil {
			hooks.OnMessage(driver, topic, payload.Slice())
		}
	}
	plugin.Exports.OnSchedule = func(name string) {
		if hooks.OnSchedule != nil {
			hooks.OnSchedule(name)
		}
	}
	plugin.Exports.OnJobComplete = dispatchJobComplete(hooks)
	plugin.Exports.OnStop = func() {
		if hooks.OnStop != nil {
			hooks.OnStop()
		}
	}
}

// SubmitHTTP はリクエストを非同期に投げ、完了時に cb を起動する。
// 受付が拒否された場合(未承認 / in-flight 上限)は cb を登録せず
// 同期の error を返す。
func SubmitHTTP(req Request, timeoutMS *uint32, cb func(*Response, error)) (uint64, error) {
	timeout := cm.None[uint32]()
	if timeoutMS != nil {
		timeout = cm.Some(*timeoutMS)
	}
	result := driverhttp.SubmitSend(req, timeout)
	if result.IsErr() {
		return 0, fmt.Errorf("submit-send: %v", result.Err())
	}
	jobID := *result.OK()
	registerPending(jobID, cb)
	return jobID, nil
}
