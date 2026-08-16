package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// missionAccepted は受注。受注地は Journal に入っていないため、直近の
// 移動イベントで覚えた場所を添える。
type missionAccepted struct {
	Faction            string `json:"Faction"`
	Name               string `json:"Name"`
	MissionID          *int64 `json:"MissionID"`
	Commodity          string `json:"Commodity"`
	Count              *int64 `json:"Count"`
	DestinationSystem  string `json:"DestinationSystem"`
	DestinationStation string `json:"DestinationStation"`
	TargetFaction      string `json:"TargetFaction"`
	Expiry             string `json:"Expiry"`
	Influence          string `json:"Influence"`
	Reputation         string `json:"Reputation"`
	Target             string `json:"Target"`
	TargetType         string `json:"TargetType"`
	KillCount          *int64 `json:"KillCount"`
	PassengerType      string `json:"PassengerType"`
	PassengerCount     *int64 `json:"PassengerCount"`
	PassengerVIPs      bool   `json:"PassengerVIPs"`
	PassengerWanted    bool   `json:"PassengerWanted"`
}

func (m missionAccepted) convert(st *State) []inara.Event {
	if m.MissionID == nil {
		return nil
	}
	return []inara.Event{inara.New("addCommanderMission", struct {
		Name              string `json:"missionName"`
		ID                int64  `json:"missionGameID"`
		Expiry            string `json:"missionExpiry,omitempty"`
		InfluenceGain     string `json:"influenceGain,omitempty"`
		ReputationGain    string `json:"reputationGain,omitempty"`
		SystemOrigin      string `json:"starsystemNameOrigin,omitempty"`
		StationOrigin     string `json:"stationNameOrigin,omitempty"`
		FactionOrigin     string `json:"minorfactionNameOrigin,omitempty"`
		SystemTarget      string `json:"starsystemNameTarget,omitempty"`
		StationTarget     string `json:"stationNameTarget,omitempty"`
		FactionTarget     string `json:"minorfactionNameTarget,omitempty"`
		Commodity         string `json:"commodityName,omitempty"`
		CommodityCount    *int64 `json:"commodityCount,omitempty"`
		TargetName        string `json:"targetName,omitempty"`
		TargetType        string `json:"targetType,omitempty"`
		KillCount         *int64 `json:"killCount,omitempty"`
		PassengerType     string `json:"passengerType,omitempty"`
		PassengerCount    *int64 `json:"passengerCount,omitempty"`
		PassengerIsVIP    bool   `json:"passengerIsVIP,omitempty"`
		PassengerIsWanted bool   `json:"passengerIsWanted,omitempty"`
	}{
		Name:              m.Name,
		ID:                *m.MissionID,
		Expiry:            m.Expiry,
		InfluenceGain:     m.Influence,
		ReputationGain:    m.Reputation,
		SystemOrigin:      st.LastSystem,
		StationOrigin:     st.LastStation,
		FactionOrigin:     m.Faction,
		SystemTarget:      m.DestinationSystem,
		StationTarget:     m.DestinationStation,
		FactionTarget:     m.TargetFaction,
		Commodity:         m.Commodity,
		CommodityCount:    m.Count,
		TargetName:        m.Target,
		TargetType:        m.TargetType,
		KillCount:         m.KillCount,
		PassengerType:     m.PassengerType,
		PassengerCount:    m.PassengerCount,
		PassengerIsVIP:    m.PassengerVIPs,
		PassengerIsWanted: m.PassengerWanted,
	})}
}

type rewardItem struct {
	Name  string `json:"Name"`
	Count int64  `json:"Count"`
}

type missionCompleted struct {
	MissionID       *int64       `json:"MissionID"`
	Donated         *int64       `json:"Donated"`
	Reward          *int64       `json:"Reward"`
	PermitsAwarded  []string     `json:"PermitsAwarded"`
	CommodityReward []rewardItem `json:"CommodityReward"`
	MaterialsReward []rewardItem `json:"MaterialsReward"`
}

func rewardItems(items []rewardItem) []inventoryItem {
	if len(items) == 0 {
		return nil
	}
	out := make([]inventoryItem, 0, len(items))
	for _, item := range items {
		out = append(out, inventoryItem{Name: item.Name, Count: item.Count})
	}
	return out
}

func (m missionCompleted) convert(*State) []inara.Event {
	if m.MissionID == nil {
		return nil
	}
	events := []inara.Event{inara.New("setCommanderMissionCompleted", struct {
		ID          int64           `json:"missionGameID"`
		Donation    *int64          `json:"donationCredits,omitempty"`
		Reward      *int64          `json:"rewardCredits,omitempty"`
		Permits     []string        `json:"rewardPermits,omitempty"`
		Commodities []inventoryItem `json:"rewardCommodities,omitempty"`
		Materials   []inventoryItem `json:"rewardMaterials,omitempty"`
	}{
		ID:          *m.MissionID,
		Donation:    m.Donated,
		Reward:      m.Reward,
		Permits:     m.PermitsAwarded,
		Commodities: rewardItems(m.CommodityReward),
		Materials:   rewardItems(m.MaterialsReward),
	})}
	// 許可証(permit)は INARA では独立したイベント。Journal に permit 専用の
	// イベントは無く、ミッション報酬としてだけ手に入るのでここから送る。
	for _, system := range m.PermitsAwarded {
		events = append(events, inara.New("addCommanderPermit", struct {
			System string `json:"starsystemName"`
		}{system}))
	}
	return events
}

type missionRef struct {
	MissionID *int64 `json:"MissionID"`
}

func (m missionRef) event(name string) []inara.Event {
	if m.MissionID == nil {
		return nil
	}
	return []inara.Event{inara.New(name, struct {
		ID int64 `json:"missionGameID"`
	}{*m.MissionID})}
}

type missionFailed missionRef

func (m missionFailed) convert(*State) []inara.Event {
	return missionRef(m).event("setCommanderMissionFailed")
}

type missionAbandoned missionRef

func (m missionAbandoned) convert(*State) []inara.Event {
	return missionRef(m).event("setCommanderMissionAbandoned")
}
