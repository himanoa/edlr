package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// journalFaction は FSDJump / Location の Factions 配列の 1 要素。
// MyReputation は -100..100 のパーセント。ポインタなのは 0(中立)と
// 「入っていない」を区別するため。
type journalFaction struct {
	Name         string   `json:"Name"`
	MyReputation *float64 `json:"MyReputation"`
}

type minorFactionReputation struct {
	Name       string  `json:"minorfactionName"`
	Reputation float64 `json:"minorfactionReputation"`
}

// minorFactionEvents は Factions から少数勢力評判のイベント列を作る。
// INARA は -1..1 の比率を受け取る。
func minorFactionEvents(factions []journalFaction) []inara.Event {
	var events []inara.Event
	for _, f := range factions {
		if f.Name == "" || f.MyReputation == nil {
			continue
		}
		events = append(events, inara.New("setCommanderReputationMinorFaction", minorFactionReputation{
			Name:       f.Name,
			Reputation: *f.MyReputation / 100,
		}))
	}
	return events
}

type fsdJump struct {
	StarSystem string           `json:"StarSystem"`
	JumpDist   float64          `json:"JumpDist"`
	StarPos    *[3]float64      `json:"StarPos"`
	Factions   []journalFaction `json:"Factions"`
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
	// ジャンプしたのでステーションからは離れている。
	st.LastStation = ""
	events := []inara.Event{inara.New("addCommanderTravelFSDJump", travelJump{
		System:   j.StarSystem,
		Distance: j.JumpDist,
		Coords:   j.StarPos,
	})}
	return append(events, minorFactionEvents(j.Factions)...)
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
	if s.StationName != "" {
		st.LastStation = s.StationName
	}
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

type location struct {
	station
	Factions []journalFaction `json:"Factions"`
}

func (l location) convert(st *State) []inara.Event {
	events := l.station.event("setCommanderTravelLocation", st)
	if events == nil {
		return nil
	}
	return append(events, minorFactionEvents(l.Factions)...)
}

// touchdown は惑星への着陸。INARA は船種を必須にしているため、Loadout などで
// 現在の船を学習するまでは送らない。古い Journal は StarSystem / Body を
// 含まないことがあり、星系は直近の移動イベントから補完する。
type touchdown struct {
	StarSystem string `json:"StarSystem"`
	Body       string `json:"Body"`
	Taxi       bool   `json:"Taxi"`
}

type travelLand struct {
	System string `json:"starsystemName"`
	Body   string `json:"starsystemBodyName"`
	Ship   string `json:"shipType"`
	ShipID int64  `json:"shipGameID"`
	IsTaxi bool   `json:"isTaxiShuttle,omitempty"`
}

func (l touchdown) convert(st *State) []inara.Event {
	if l.StarSystem != "" {
		st.LastSystem = l.StarSystem
	}
	system := st.LastSystem
	if system == "" || l.Body == "" || st.ShipType == "" || st.ShipID == nil {
		return nil
	}
	return []inara.Event{inara.New("addCommanderTravelLand", travelLand{
		System: system,
		Body:   l.Body,
		Ship:   st.ShipType,
		ShipID: *st.ShipID,
		IsTaxi: l.Taxi,
	})}
}
