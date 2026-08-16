package mapping

import "encoding/json"

// persisted は State のディスク表現。フィールドの意味は State を参照。
type persisted struct {
	CommanderName string             `json:"commanderName,omitempty"`
	FrontierID    string             `json:"frontierID,omitempty"`
	LastSystem    string             `json:"lastSystem,omitempty"`
	LastStation   string             `json:"lastStation,omitempty"`
	ShipType      string             `json:"shipType,omitempty"`
	ShipID        *int64             `json:"shipID,omitempty"`
	Ranks         map[string]int     `json:"ranks,omitempty"`
	Progress      map[string]float64 `json:"progress,omitempty"`
	GameVersion   string             `json:"gameVersion,omitempty"`
}

// Marshal は State を永続化用の JSON にする。
func (s *State) Marshal() ([]byte, error) {
	return json.Marshal(persisted{
		CommanderName: s.CommanderName,
		FrontierID:    s.FrontierID,
		LastSystem:    s.LastSystem,
		LastStation:   s.LastStation,
		ShipType:      s.ShipType,
		ShipID:        s.ShipID,
		Ranks:         s.ranks,
		Progress:      s.progress,
		GameVersion:   s.gameVersion,
	})
}

// UnmarshalState は保存済み JSON から State を復元する。読めない・壊れている
// 場合は空の State を返す(保存前の初回起動と同じ扱いにする)。
func UnmarshalState(data []byte) *State {
	var p persisted
	if err := json.Unmarshal(data, &p); err != nil {
		return NewState()
	}
	s := NewState()
	s.CommanderName = p.CommanderName
	s.FrontierID = p.FrontierID
	s.LastSystem = p.LastSystem
	s.LastStation = p.LastStation
	s.ShipType = p.ShipType
	s.ShipID = p.ShipID
	for k, v := range p.Ranks {
		s.ranks[k] = v
	}
	for k, v := range p.Progress {
		s.progress[k] = v
	}
	s.gameVersion = p.GameVersion
	return s
}
