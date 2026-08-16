package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// shipRef は INARA が船のイベント全種で必須にする識別子。
type shipRef struct {
	Type string `json:"shipType"`
	ID   int64  `json:"shipGameID"`
}

// shipData は setCommanderShip のペイロード。省略可能なフィールドは
// 「Journal に入っていない」を送らないため全部 omitempty / ポインタ。
type shipData struct {
	shipRef
	Name          string   `json:"shipName,omitempty"`
	Ident         string   `json:"shipIdent,omitempty"`
	IsCurrent     bool     `json:"isCurrentShip,omitempty"`
	IsHot         bool     `json:"isHot,omitempty"`
	HullValue     *int64   `json:"shipHullValue,omitempty"`
	ModulesValue  *int64   `json:"shipModulesValue,omitempty"`
	RebuyCost     *int64   `json:"shipRebuyCost,omitempty"`
	MaxJumpRange  *float64 `json:"shipMaxJumpRange,omitempty"`
	CargoCapacity *int64   `json:"shipCargoCapacity,omitempty"`
	System        string   `json:"starsystemName,omitempty"`
	Station       string   `json:"stationName,omitempty"`
	MarketID      *int64   `json:"marketID,omitempty"`
}

// --- Loadout ---

// journalEngineering は Journal の Engineering ブロック。
type journalEngineering struct {
	BlueprintName      string  `json:"BlueprintName"`
	Level              int     `json:"Level"`
	Quality            float64 `json:"Quality"`
	ExperimentalEffect string  `json:"ExperimentalEffect_Localised"`
	Modifiers          []struct {
		Label         string  `json:"Label"`
		Value         float64 `json:"Value"`
		OriginalValue float64 `json:"OriginalValue"`
		LessIsGood    int     `json:"LessIsGood"`
	} `json:"Modifiers"`
}

type loadoutEngineering struct {
	BlueprintName      string  `json:"blueprintName"`
	BlueprintLevel     int     `json:"blueprintLevel"`
	BlueprintQuality   float64 `json:"blueprintQuality"`
	ExperimentalEffect string  `json:"experimentalEffect,omitempty"`
	Modifiers          []struct {
		Name          string  `json:"name"`
		Value         float64 `json:"value"`
		OriginalValue float64 `json:"originalValue"`
		LessIsGood    bool    `json:"lessIsGood"`
	} `json:"modifiers,omitempty"`
}

func (e *journalEngineering) toINARA() *loadoutEngineering {
	if e == nil {
		return nil
	}
	out := &loadoutEngineering{
		BlueprintName:      e.BlueprintName,
		BlueprintLevel:     e.Level,
		BlueprintQuality:   e.Quality,
		ExperimentalEffect: e.ExperimentalEffect,
	}
	for _, m := range e.Modifiers {
		out.Modifiers = append(out.Modifiers, struct {
			Name          string  `json:"name"`
			Value         float64 `json:"value"`
			OriginalValue float64 `json:"originalValue"`
			LessIsGood    bool    `json:"lessIsGood"`
		}{m.Label, m.Value, m.OriginalValue, m.LessIsGood != 0})
	}
	return out
}

type loadout struct {
	Ship          string   `json:"Ship"`
	ShipID        *int64   `json:"ShipID"`
	ShipName      string   `json:"ShipName"`
	ShipIdent     string   `json:"ShipIdent"`
	HullValue     *int64   `json:"HullValue"`
	ModulesValue  *int64   `json:"ModulesValue"`
	Rebuy         *int64   `json:"Rebuy"`
	MaxJumpRange  *float64 `json:"MaxJumpRange"`
	CargoCapacity *int64   `json:"CargoCapacity"`
	Hot           bool     `json:"Hot"`
	Modules       []struct {
		Slot         string              `json:"Slot"`
		Item         string              `json:"Item"`
		On           bool                `json:"On"`
		Priority     int                 `json:"Priority"`
		Health       float64             `json:"Health"`
		Value        *int64              `json:"Value"`
		AmmoInClip   *int64              `json:"AmmoInClip"`
		AmmoInHopper *int64              `json:"AmmoInHopper"`
		Engineering  *journalEngineering `json:"Engineering"`
	} `json:"Modules"`
}

type loadoutModule struct {
	SlotName     string              `json:"slotName"`
	ItemName     string              `json:"itemName"`
	ItemValue    *int64              `json:"itemValue,omitempty"`
	ItemHealth   float64             `json:"itemHealth"`
	IsOn         bool                `json:"isOn"`
	ItemPriority int                 `json:"itemPriority"`
	AmmoClip     *int64              `json:"itemAmmoClip,omitempty"`
	AmmoHopper   *int64              `json:"itemAmmoHopper,omitempty"`
	Engineering  *loadoutEngineering `json:"engineering,omitempty"`
}

func (l loadout) convert(st *State) []inara.Event {
	if l.Ship == "" || l.ShipID == nil {
		return nil
	}
	st.ShipType = l.Ship
	st.ShipID = l.ShipID

	ref := shipRef{Type: l.Ship, ID: *l.ShipID}
	modules := make([]loadoutModule, 0, len(l.Modules))
	for _, m := range l.Modules {
		modules = append(modules, loadoutModule{
			SlotName:     m.Slot,
			ItemName:     m.Item,
			ItemValue:    m.Value,
			ItemHealth:   m.Health,
			IsOn:         m.On,
			ItemPriority: m.Priority,
			AmmoClip:     m.AmmoInClip,
			AmmoHopper:   m.AmmoInHopper,
			Engineering:  m.Engineering.toINARA(),
		})
	}
	return []inara.Event{
		inara.New("setCommanderShip", shipData{
			shipRef:       ref,
			Name:          l.ShipName,
			Ident:         l.ShipIdent,
			IsCurrent:     true,
			IsHot:         l.Hot,
			HullValue:     l.HullValue,
			ModulesValue:  l.ModulesValue,
			RebuyCost:     l.Rebuy,
			MaxJumpRange:  l.MaxJumpRange,
			CargoCapacity: l.CargoCapacity,
		}),
		inara.New("setCommanderShipLoadout", struct {
			shipRef
			Loadout []loadoutModule `json:"shipLoadout"`
		}{ref, modules}),
	}
}

// --- Shipyard 系 ---

type shipyardNew struct {
	ShipType  string `json:"ShipType"`
	NewShipID *int64 `json:"NewShipID"`
}

func (s shipyardNew) convert(st *State) []inara.Event {
	if s.ShipType == "" || s.NewShipID == nil {
		return nil
	}
	// 買った船にそのまま乗り込むので、現在の船として覚える。
	st.ShipType = s.ShipType
	st.ShipID = s.NewShipID
	return []inara.Event{inara.New("addCommanderShip", shipRef{Type: s.ShipType, ID: *s.NewShipID})}
}

type shipyardSell struct {
	ShipType   string `json:"ShipType"`
	SellShipID *int64 `json:"SellShipID"`
}

func (s shipyardSell) convert(*State) []inara.Event {
	if s.ShipType == "" || s.SellShipID == nil {
		return nil
	}
	return []inara.Event{inara.New("delCommanderShip", shipRef{Type: s.ShipType, ID: *s.SellShipID})}
}

type shipyardSwap struct {
	ShipType string `json:"ShipType"`
	ShipID   *int64 `json:"ShipID"`
}

func (s shipyardSwap) convert(st *State) []inara.Event {
	if s.ShipType == "" || s.ShipID == nil {
		return nil
	}
	st.ShipType = s.ShipType
	st.ShipID = s.ShipID
	return []inara.Event{inara.New("setCommanderShip", shipData{
		shipRef:   shipRef{Type: s.ShipType, ID: *s.ShipID},
		IsCurrent: true,
	})}
}

// shipyardTransfer は船の呼び寄せ。Journal の System は船が「今ある」場所で、
// INARA が求めるのは輸送先(= 自分が今いるステーション)。ステーションが
// 未学習なら送りようがないので送らない。
type shipyardTransfer struct {
	ShipType     string `json:"ShipType"`
	ShipID       *int64 `json:"ShipID"`
	TransferTime *int64 `json:"TransferTime"`
}

func (s shipyardTransfer) convert(st *State) []inara.Event {
	if s.ShipType == "" || s.ShipID == nil || st.LastSystem == "" || st.LastStation == "" {
		return nil
	}
	return []inara.Event{inara.New("setCommanderShipTransfer", struct {
		shipRef
		System       string `json:"starsystemName"`
		Station      string `json:"stationName"`
		TransferTime *int64 `json:"transferTime,omitempty"`
	}{shipRef{Type: s.ShipType, ID: *s.ShipID}, st.LastSystem, st.LastStation, s.TransferTime})}
}

type setUserShipName struct {
	Ship      string `json:"Ship"`
	ShipID    *int64 `json:"ShipID"`
	ShipName  string `json:"UserShipName"`
	ShipIdent string `json:"UserShipId"`
}

func (s setUserShipName) convert(*State) []inara.Event {
	if s.Ship == "" || s.ShipID == nil {
		return nil
	}
	return []inara.Event{inara.New("setCommanderShip", shipData{
		shipRef: shipRef{Type: s.Ship, ID: *s.ShipID},
		Name:    s.ShipName,
		Ident:   s.ShipIdent,
	})}
}

// --- StoredShips / StoredModules ---

type storedShip struct {
	ShipID     *int64 `json:"ShipID"`
	ShipType   string `json:"ShipType"`
	Name       string `json:"Name"`
	Hot        bool   `json:"Hot"`
	StarSystem string `json:"StarSystem"`
}

type storedShips struct {
	StationName string       `json:"StationName"`
	StarSystem  string       `json:"StarSystem"`
	MarketID    *int64       `json:"MarketID"`
	ShipsHere   []storedShip `json:"ShipsHere"`
	ShipsRemote []storedShip `json:"ShipsRemote"`
}

func (s storedShips) convert(*State) []inara.Event {
	var events []inara.Event
	add := func(ship storedShip, system, station string, marketID *int64) {
		if ship.ShipType == "" || ship.ShipID == nil {
			return
		}
		events = append(events, inara.New("setCommanderShip", shipData{
			shipRef:  shipRef{Type: ship.ShipType, ID: *ship.ShipID},
			Name:     ship.Name,
			IsHot:    ship.Hot,
			System:   system,
			Station:  station,
			MarketID: marketID,
		}))
	}
	for _, ship := range s.ShipsHere {
		add(ship, s.StarSystem, s.StationName, s.MarketID)
	}
	for _, ship := range s.ShipsRemote {
		add(ship, ship.StarSystem, "", nil)
	}
	return events
}

type storedModules struct {
	Items []struct {
		Name                  string  `json:"Name"`
		StarSystem            string  `json:"StarSystem"`
		MarketID              *int64  `json:"MarketID"`
		BuyPrice              int64   `json:"BuyPrice"`
		Hot                   bool    `json:"Hot"`
		EngineerModifications string  `json:"EngineerModifications"`
		Level                 int     `json:"Level"`
		Quality               float64 `json:"Quality"`
	} `json:"Items"`
}

type storageModule struct {
	ItemName    string              `json:"itemName"`
	ItemValue   int64               `json:"itemValue"`
	IsHot       bool                `json:"isHot,omitempty"`
	System      string              `json:"starsystemName,omitempty"`
	MarketID    *int64              `json:"marketID,omitempty"`
	Engineering *loadoutEngineering `json:"engineering,omitempty"`
}

func (s storedModules) convert(*State) []inara.Event {
	if len(s.Items) == 0 {
		return nil
	}
	modules := make([]storageModule, 0, len(s.Items))
	for _, item := range s.Items {
		m := storageModule{
			ItemName:  item.Name,
			ItemValue: item.BuyPrice,
			IsHot:     item.Hot,
			System:    item.StarSystem,
			MarketID:  item.MarketID,
		}
		if item.EngineerModifications != "" {
			m.Engineering = &loadoutEngineering{
				BlueprintName:    item.EngineerModifications,
				BlueprintLevel:   item.Level,
				BlueprintQuality: item.Quality,
			}
		}
		modules = append(modules, m)
	}
	return []inara.Event{inara.New("setCommanderStorageModules", modules)}
}
