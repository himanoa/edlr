// tutorial-tracker は docs/plugin-tutorial-tinygo.md の 6 章で作るドライバ。
//
// プラグインから `visit` で受け取った星系名を数え、`last-system`
// (`retain = true`)へ流し直す。ドライバはプロセス内に 1 インスタンスしか
// 居ないので、複数のプラグインが publish しても数はこの 1 つに集まる。
package main

import (
	"encoding/json"
	"fmt"

	"go.bytecodealliance.org/cm"

	bushost "github.com/himanoa/edlr/examples/drivers/tutorial-tracker-go/gen/edlr/plugin/bus-host"
	driver "github.com/himanoa/edlr/examples/drivers/tutorial-tracker-go/gen/edlr/plugin/driver"
	hostlog "github.com/himanoa/edlr/examples/drivers/tutorial-tracker-go/gen/edlr/plugin/host-log"
)

var count int

func init() {
	driver.Exports.Init = onInit
	driver.Exports.OnMessage = onMessage
}

// main は TinyGo がコンポーネントをビルドするために要る。
func main() {}

func onInit() {
	hostlog.Log(hostlog.LevelInfo, "tutorial-tracker started")
}

func onMessage(from string, topic string, payload cm.List[uint8]) {
	// ドライバは driver.toml の `[[topics]]` に書いた分しか受け取らないが、
	// トピックを増やしたときのために分岐しておく。
	if topic != "visit" {
		return
	}

	system := string(payload.Slice())
	count++
	// デーモンのログレベルは INFO 固定なので、debug では何も見えない。
	hostlog.Log(hostlog.LevelInfo,
		fmt.Sprintf("visit #%d from %s: %s", count, from, system))

	body, err := json.Marshal(struct {
		System string `json:"system"`
		Count  int    `json:"count"`
	}{System: system, Count: count})
	if err != nil {
		hostlog.Log(hostlog.LevelWarn, "could not encode the payload: "+err.Error())
		return
	}

	emitted := bushost.Emit("last-system", cm.ToList(body))
	if e := emitted.Err(); e != nil {
		hostlog.Log(hostlog.LevelWarn, "emit failed: "+e.String())
	}
}
