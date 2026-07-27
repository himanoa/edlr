// Package mapping は Journal イベントを INARA API v1 のイベントへ変換する。
//
// 対応するイベントは handlers がすべて。ここに無い Journal イベントは
// デコードすらせずに捨てる。manifest.toml の `events` と handlers のキーが
// 一致することは manifest_test.go が検証する。
package mapping

import (
	"encoding/json"
	"fmt"
	"sort"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

// converter は 1 つの Journal イベントのペイロードに対応する型。
//
// convert は State を書き換えてよい(コマンダー名の学習や、直近の星系の
// 記録がこれにあたる)。送信するものが無ければ nil を返す。
type converter interface {
	convert(st *State) []inara.Event
}

// handler は 1 つの Journal イベントの扱い。
type handler struct {
	// convert はペイロードのデコードと変換(送信対象でないイベントは nil)
	convert func(raw json.RawMessage, st *State) ([]inara.Event, error)
	// flushLive は live モードで即時フラッシュを促すか(Shutdown のみ true)
	flushLive bool
}

// handlerFor は converter を実装した型 T へデコードして変換する handler を作る。
func handlerFor[T converter]() handler {
	return handler{
		convert: func(raw json.RawMessage, st *State) ([]inara.Event, error) {
			var v T
			if err := json.Unmarshal(raw, &v); err != nil {
				return nil, err
			}
			return v.convert(st), nil
		},
	}
}

var handlers = map[string]handler{
	"Commander":   handlerFor[commander](),
	"LoadGame":    handlerFor[loadGame](),
	"FSDJump":     handlerFor[fsdJump](),
	"CarrierJump": handlerFor[carrierJump](),
	"Docked":      handlerFor[docked](),
	"Location":    handlerFor[location](),
}

// Result は Journal イベント 1 件を扱った結果。
type Result struct {
	Events    []inara.Event
	FlushLive bool
}

// Convert は Journal イベント 1 件を INARA のイベントへ変換する。
// 未知のイベントは空の Result を返す(エラーではない)。
func Convert(name, timestamp string, payload json.RawMessage, st *State) (Result, error) {
	h, ok := handlers[name]
	if !ok {
		return Result{}, nil
	}

	res := Result{FlushLive: h.flushLive}
	if h.convert == nil {
		return res, nil
	}

	events, err := h.convert(payload, st)
	if err != nil {
		return res, fmt.Errorf("%s: %w", name, err)
	}
	// timestamp はここでまとめて埋める。個々のマッパーに配って回らずに済む。
	for i := range events {
		events[i].Timestamp = timestamp
	}
	res.Events = events
	return res, nil
}

// Names は購読すべき Journal イベント名を返す。manifest.toml の `events` と
// 一致していること。
func Names() []string {
	names := make([]string, 0, len(handlers))
	for name := range handlers {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}
