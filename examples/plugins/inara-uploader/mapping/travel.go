package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

type fsdJump struct {
	StarSystem string      `json:"StarSystem"`
	JumpDist   float64     `json:"JumpDist"`
	StarPos    *[3]float64 `json:"StarPos"`
}

type travelJump struct {
	System   string      `json:"starsystemName"`
	Distance float64     `json:"jumpDistance,omitempty"`
	Coords   *[3]float64 `json:"starsystemCoords,omitempty"`
}

func (j fsdJump) convert(st *State) []inara.Event {
	if j.StarSystem == "" {
		return nil
	}
	st.LastSystem = j.StarSystem
	return []inara.Event{inara.New("addCommanderTravelFSDJump", travelJump{
		System:   j.StarSystem,
		Distance: j.JumpDist,
		Coords:   j.StarPos,
	})}
}

// station は星系と(あれば)ステーションを持つ Journal イベントの共通部分。
// CarrierJump / Docked / Location はいずれもこの形。
type station struct {
	StarSystem  string `json:"StarSystem"`
	StationName string `json:"StationName"`
	MarketID    *int64 `json:"MarketID"`
}

type travelStation struct {
	System   string `json:"starsystemName"`
	Station  string `json:"stationName,omitempty"`
	MarketID *int64 `json:"marketID,omitempty"`
}

func (s station) event(name string, st *State) []inara.Event {
	if s.StarSystem == "" {
		return nil
	}
	st.LastSystem = s.StarSystem
	return []inara.Event{inara.New(name, travelStation{
		System:   s.StarSystem,
		Station:  s.StationName,
		MarketID: s.MarketID,
	})}
}

type carrierJump station

func (c carrierJump) convert(st *State) []inara.Event {
	return station(c).event("addCommanderTravelCarrierJump", st)
}

type docked station

func (d docked) convert(st *State) []inara.Event {
	// ドックはステーション名が要る。無ければ送らない。
	if d.StationName == "" {
		return nil
	}
	return station(d).event("addCommanderTravelDock", st)
}

type location station

func (l location) convert(st *State) []inara.Event {
	return station(l).event("setCommanderTravelLocation", st)
}
