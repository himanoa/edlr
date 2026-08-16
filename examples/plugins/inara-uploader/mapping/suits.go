package mapping

import (
	"strconv"
	"strings"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

// suitLoadout は SuitLoadout / SwitchSuitLoadout / CreateSuitLoadout の共通形。
// いずれも「この装備一式が今どうなっているか」の全量なので、同じ
// setCommanderSuitLoadout として送る。
type suitLoadout struct {
	SuitID      *int64   `json:"SuitID"`
	SuitName    string   `json:"SuitName"`
	SuitMods    []string `json:"SuitMods"`
	LoadoutID   *int64   `json:"LoadoutID"`
	LoadoutName string   `json:"LoadoutName"`
	Modules     []struct {
		SlotName     string   `json:"SlotName"`
		SuitModuleID *int64   `json:"SuitModuleID"`
		ModuleName   string   `json:"ModuleName"`
		Class        int      `json:"Class"`
		WeaponMods   []string `json:"WeaponMods"`
	} `json:"Modules"`
}

type suitModule struct {
	SlotName string   `json:"slotName"`
	ItemName string   `json:"itemName"`
	ItemID   *int64   `json:"itemGameID,omitempty"`
	Class    int      `json:"itemClass,omitempty"`
	Mods     []string `json:"itemMods,omitempty"`
}

func (s suitLoadout) convert(*State) []inara.Event {
	if s.LoadoutID == nil || s.SuitID == nil || s.SuitName == "" {
		return nil
	}
	modules := make([]suitModule, 0, len(s.Modules))
	for _, m := range s.Modules {
		modules = append(modules, suitModule{
			SlotName: m.SlotName,
			ItemName: m.ModuleName,
			ItemID:   m.SuitModuleID,
			Class:    m.Class,
			Mods:     m.WeaponMods,
		})
	}
	return []inara.Event{inara.New("setCommanderSuitLoadout", struct {
		LoadoutID   int64        `json:"loadoutGameID"`
		SuitType    string       `json:"suitType"`
		SuitID      int64        `json:"suitGameID"`
		LoadoutName string       `json:"loadoutName,omitempty"`
		SuitMods    []string     `json:"suitMods,omitempty"`
		Loadout     []suitModule `json:"suitLoadout,omitempty"`
	}{*s.LoadoutID, s.SuitName, *s.SuitID, s.LoadoutName, s.SuitMods, modules})}
}

type renameSuitLoadout struct {
	SuitID      *int64 `json:"SuitID"`
	SuitName    string `json:"SuitName"`
	LoadoutID   *int64 `json:"LoadoutID"`
	LoadoutName string `json:"LoadoutName"`
}

func (r renameSuitLoadout) convert(*State) []inara.Event {
	if r.LoadoutID == nil {
		return nil
	}
	return []inara.Event{inara.New("updateCommanderSuitLoadout", struct {
		LoadoutID   int64  `json:"loadoutGameID"`
		SuitType    string `json:"suitType,omitempty"`
		SuitID      *int64 `json:"suitGameID,omitempty"`
		LoadoutName string `json:"loadoutName,omitempty"`
	}{*r.LoadoutID, r.SuitName, r.SuitID, r.LoadoutName})}
}

type deleteSuitLoadout struct {
	LoadoutID *int64 `json:"LoadoutID"`
}

func (d deleteSuitLoadout) convert(*State) []inara.Event {
	if d.LoadoutID == nil {
		return nil
	}
	return []inara.Event{inara.New("delCommanderSuitLoadout", struct {
		LoadoutID int64 `json:"loadoutGameID"`
	}{*d.LoadoutID})}
}

// communityGoal はコミュニティゴールの一覧。ゴールそのものの情報
// (setCommunityGoal)と自分の貢献(setCommanderCommunityGoalProgress)を
// ゴールごとに送る。
type communityGoal struct {
	CurrentGoals []struct {
		CGID                 *int64 `json:"CGID"`
		Title                string `json:"Title"`
		SystemName           string `json:"SystemName"`
		MarketName           string `json:"MarketName"`
		Expiry               string `json:"Expiry"`
		IsComplete           bool   `json:"IsComplete"`
		CurrentTotal         int64  `json:"CurrentTotal"`
		PlayerContribution   int64  `json:"PlayerContribution"`
		NumContributors      int64  `json:"NumContributors"`
		TopRankSize          *int64 `json:"TopRankSize"`
		PlayerInTopRank      *bool  `json:"PlayerInTopRank"`
		TierReached          string `json:"TierReached"`
		PlayerPercentileBand *int64 `json:"PlayerPercentileBand"`
		Bonus                *int64 `json:"Bonus"`
	} `json:"CurrentGoals"`
}

// tierNumber は Journal の "Tier 2" 形式から数値を取り出す(INARA は整数)。
func tierNumber(tier string) *int {
	n, err := strconv.Atoi(strings.TrimPrefix(tier, "Tier "))
	if err != nil {
		return nil
	}
	return &n
}

func (c communityGoal) convert(*State) []inara.Event {
	var events []inara.Event
	for _, g := range c.CurrentGoals {
		if g.CGID == nil || g.Title == "" {
			continue
		}
		events = append(events,
			inara.New("setCommunityGoal", struct {
				ID           int64  `json:"communitygoalGameID"`
				Name         string `json:"communitygoalName"`
				System       string `json:"starsystemName"`
				Station      string `json:"stationName"`
				Expiry       string `json:"goalExpiry"`
				TierReached  *int   `json:"tierReached,omitempty"`
				TopRankSize  *int64 `json:"topRankSize,omitempty"`
				IsCompleted  bool   `json:"isCompleted"`
				Contributors int64  `json:"contributorsNum"`
				Total        int64  `json:"contributionsTotal"`
				Bonus        *int64 `json:"completionBonus,omitempty"`
			}{*g.CGID, g.Title, g.SystemName, g.MarketName, g.Expiry,
				tierNumber(g.TierReached), g.TopRankSize, g.IsComplete,
				g.NumContributors, g.CurrentTotal, g.Bonus}),
			inara.New("setCommanderCommunityGoalProgress", struct {
				ID             int64  `json:"communitygoalGameID"`
				Contribution   int64  `json:"contribution"`
				PercentileBand *int64 `json:"percentileBand,omitempty"`
				IsTopRank      *bool  `json:"isTopRank,omitempty"`
			}{*g.CGID, g.PlayerContribution, g.PlayerPercentileBand, g.PlayerInTopRank}),
		)
	}
	return events
}

// friends は友達リストの変化。Online / Offline などの在席通知はリストの
// 変化ではないので送らない。
type friends struct {
	Status string `json:"Status"`
	Name   string `json:"Name"`
}

func (f friends) convert(*State) []inara.Event {
	if f.Name == "" {
		return nil
	}
	var name string
	switch f.Status {
	case "Added":
		name = "addCommanderFriend"
	case "Lost":
		name = "delCommanderFriend"
	default:
		return nil
	}
	return []inara.Event{inara.New(name, struct {
		Commander string `json:"commanderName"`
	}{f.Name})}
}
