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
	"Touchdown":   handlerFor[touchdown](),
	"Rank":        handlerFor[rank](),
	// Promotion は Rank と同じ形(昇進した分野だけが入る)なので流用する。
	"Promotion":          handlerFor[rank](),
	"Powerplay":          handlerFor[powerplay](),
	"Cargo":              handlerFor[cargo](),
	"Progress":           handlerFor[progress](),
	"Reputation":         handlerFor[reputation](),
	"EngineerProgress":   handlerFor[engineerProgress](),
	"Materials":          handlerFor[materials](),
	"Statistics":         handlerFor[statistics](),
	"Died":               handlerFor[died](),
	"PVPKill":            handlerFor[pvpKill](),
	"Interdiction":       handlerFor[interdiction](),
	"Interdicted":        handlerFor[interdicted](),
	"EscapeInterdiction": handlerFor[escapeInterdiction](),
	"Loadout":            handlerFor[loadout](),
	"ShipyardNew":        handlerFor[shipyardNew](),
	"ShipyardSell":       handlerFor[shipyardSell](),
	"ShipyardSwap":       handlerFor[shipyardSwap](),
	"ShipyardTransfer":   handlerFor[shipyardTransfer](),
	"SetUserShipName":    handlerFor[setUserShipName](),
	"StoredShips":        handlerFor[storedShips](),
	"StoredModules":      handlerFor[storedModules](),
	"MissionAccepted":    handlerFor[missionAccepted](),
	"MissionCompleted":   handlerFor[missionCompleted](),
	"MissionFailed":      handlerFor[missionFailed](),
	"MissionAbandoned":   handlerFor[missionAbandoned](),
	// SuitLoadout / SwitchSuitLoadout / CreateSuitLoadout はいずれも装備一式の
	// 全量なので同じ変換を使う。
	"SuitLoadout":       handlerFor[suitLoadout](),
	"SwitchSuitLoadout": handlerFor[suitLoadout](),
	"CreateSuitLoadout": handlerFor[suitLoadout](),
	"RenameSuitLoadout": handlerFor[renameSuitLoadout](),
	"DeleteSuitLoadout": handlerFor[deleteSuitLoadout](),
	"CommunityGoal":     handlerFor[communityGoal](),
	"Friends":           handlerFor[friends](),
	// Shutdown は送るものが無く、live モードでの即時フラッシュだけを促す。
	"Shutdown": {flushLive: true},
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
	// INARA は Live(4.0 以降、beta 除く)のデータしか受け付けない。変換
	// (= 学習込み)を済ませてから、Live と確認できていないセッションの
	// イベントはここでまとめて捨てる。LoadGame 自身の出力もゲート対象
	// (Legacy の LoadGame が学習した直後にその Credits を送らない)。
	if !st.liveAllowed() {
		return res, nil
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
