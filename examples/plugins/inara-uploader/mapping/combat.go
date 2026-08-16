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

// pvpKill は PvP での撃墜。INARA は星系名を必須にしているが、Journal の
// PVPKill は星系を含まないため、直近の移動イベントで覚えた星系を添える
// (未学習なら送らない)。
type pvpKill struct {
	Victim string `json:"Victim"`
}

func (k pvpKill) convert(st *State) []inara.Event {
	if st.LastSystem == "" || k.Victim == "" {
		return nil
	}
	return []inara.Event{inara.New("addCommanderCombatKill", struct {
		System   string `json:"starsystemName"`
		Opponent string `json:"opponentName"`
	}{st.LastSystem, k.Victim})}
}

// interdictionData は interdiction 3 兄弟で共通の INARA ペイロード。
type interdictionData struct {
	System   string `json:"starsystemName"`
	Opponent string `json:"opponentName,omitempty"`
	IsPlayer bool   `json:"isPlayer"`
	IsSubmit *bool  `json:"isSubmit,omitempty"`
	Success  *bool  `json:"isSuccess,omitempty"`
}

// interdiction は自分が相手をインターディクトした側。NPC 相手だと
// Interdicted が空で Power / Faction に名前が入ることがある。
type interdiction struct {
	Success     bool   `json:"Success"`
	IsPlayer    bool   `json:"IsPlayer"`
	Interdicted string `json:"Interdicted"`
	Power       string `json:"Power"`
	Faction     string `json:"Faction"`
}

func (i interdiction) convert(st *State) []inara.Event {
	if st.LastSystem == "" {
		return nil
	}
	opponent := i.Interdicted
	if opponent == "" {
		opponent = i.Power
	}
	if opponent == "" {
		opponent = i.Faction
	}
	return []inara.Event{inara.New("addCommanderCombatInterdiction", interdictionData{
		System:   st.LastSystem,
		Opponent: opponent,
		IsPlayer: i.IsPlayer,
		Success:  &i.Success,
	})}
}

// interdicted は自分がインターディクトされた側。
type interdicted struct {
	Submitted   bool   `json:"Submitted"`
	Interdictor string `json:"Interdictor"`
	IsPlayer    bool   `json:"IsPlayer"`
}

func (i interdicted) convert(st *State) []inara.Event {
	if st.LastSystem == "" {
		return nil
	}
	return []inara.Event{inara.New("addCommanderCombatInterdicted", interdictionData{
		System:   st.LastSystem,
		Opponent: i.Interdictor,
		IsPlayer: i.IsPlayer,
		IsSubmit: &i.Submitted,
	})}
}

// escapeInterdiction はインターディクトから逃げ切った側。
type escapeInterdiction struct {
	Interdictor string `json:"Interdictor"`
	IsPlayer    bool   `json:"IsPlayer"`
}

func (e escapeInterdiction) convert(st *State) []inara.Event {
	if st.LastSystem == "" {
		return nil
	}
	return []inara.Event{inara.New("addCommanderCombatInterdictionEscape", interdictionData{
		System:   st.LastSystem,
		Opponent: e.Interdictor,
		IsPlayer: e.IsPlayer,
	})}
}
