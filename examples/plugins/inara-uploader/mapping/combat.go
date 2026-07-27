package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// died は撃墜。Journal の Died は星系名を含まないため、直近の移動イベントで
// 覚えた星系を添える。相手は単独なら KillerName、ウイングなら Killers。
type died struct {
	KillerName string `json:"KillerName"`
	Killers    []struct {
		Name string `json:"Name"`
	} `json:"Killers"`
}

type combatDeath struct {
	System   string `json:"starsystemName,omitempty"`
	Opponent string `json:"opponentName,omitempty"`
}

func (d died) convert(st *State) []inara.Event {
	data := combatDeath{System: st.LastSystem, Opponent: d.KillerName}
	if data.Opponent == "" && len(d.Killers) > 0 {
		data.Opponent = d.Killers[0].Name
	}
	return []inara.Event{inara.New("addCommanderCombatDeath", data)}
}
