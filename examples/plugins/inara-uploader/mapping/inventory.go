package mapping

import (
	"encoding/json"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

type materialItem struct {
	Name  string `json:"Name"`
	Count *int64 `json:"Count"`
}

// materials は素材在庫。Journal は Raw / Manufactured / Encoded に分かれるが、
// INARA は種別を区別しない 1 本のリストを受け取る。
type materials struct {
	Raw          []materialItem `json:"Raw"`
	Manufactured []materialItem `json:"Manufactured"`
	Encoded      []materialItem `json:"Encoded"`
}

type inventoryItem struct {
	Name  string `json:"itemName"`
	Count int64  `json:"itemCount"`
}

func (m materials) convert(*State) []inara.Event {
	var items []inventoryItem
	for _, list := range [][]materialItem{m.Raw, m.Manufactured, m.Encoded} {
		for _, item := range list {
			if item.Name == "" || item.Count == nil {
				continue
			}
			items = append(items, inventoryItem{Name: item.Name, Count: *item.Count})
		}
	}
	if len(items) == 0 {
		return nil
	}
	return []inara.Event{inara.New("setCommanderInventoryMaterials", items)}
}

// statistics は Journal の中身をそのまま送る(INARA が同じ構造を受け付ける)。
// イベント本体のメタデータだけ落とす。
type statistics map[string]json.RawMessage

func (s statistics) convert(*State) []inara.Event {
	data := make(map[string]json.RawMessage, len(s))
	for key, value := range s {
		if key == "timestamp" || key == "event" {
			continue
		}
		data[key] = value
	}
	if len(data) == 0 {
		return nil
	}
	return []inara.Event{inara.New("setCommanderGameStatistics", data)}
}
