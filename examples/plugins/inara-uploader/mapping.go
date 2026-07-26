package main

// Journal イベント → INARA API イベントの変換。
//
// 対応しているイベントは README の「対応イベント」表がすべて。未対応の
// Journal イベントは黙って捨てる(空スライスを返す)。1 つの Journal
// イベントが複数の INARA イベントになることがある(Rank / Materials など)。

// rankKeys は Journal の Rank/Progress のキーと INARA の rankName の対応。
// INARA 側の名前は小文字。
var rankKeys = map[string]string{
	"Combat":       "combat",
	"Trade":        "trade",
	"Explore":      "exploration",
	"CQC":          "cqc",
	"Soldier":      "mercenary",
	"Exobiologist": "exobiologist",
	"Empire":       "empire",
	"Federation":   "federation",
}

// majorFactions は Reputation イベントのキー。
var majorFactions = []string{"Empire", "Federation", "Alliance", "Independent"}

func mapEvent(name, timestamp string, p map[string]any, st *state) []inaraEvent {
	switch name {
	case "LoadGame":
		return mapLoadGame(timestamp, p)
	case "FSDJump":
		return mapFSDJump(timestamp, p, st)
	case "CarrierJump":
		return mapCarrierJump(timestamp, p, st)
	case "Docked":
		return mapDocked(timestamp, p, st)
	case "Location":
		return mapLocation(timestamp, p, st)
	case "Rank":
		return mapRank(timestamp, p, st)
	case "Progress":
		return mapProgress(timestamp, p, st)
	case "Reputation":
		return mapReputation(timestamp, p)
	case "EngineerProgress":
		return mapEngineerProgress(timestamp, p)
	case "Materials":
		return mapMaterials(timestamp, p)
	case "Statistics":
		return mapStatistics(timestamp, p)
	case "Died":
		return mapDied(timestamp, p, st)
	}
	return nil
}

func mapLoadGame(ts string, p map[string]any) []inaraEvent {
	credits, ok := num(p, "Credits")
	if !ok {
		return nil
	}
	data := map[string]any{"commanderCredits": int64(credits)}
	if loan, ok := num(p, "Loan"); ok {
		data["commanderLoan"] = int64(loan)
	}
	return []inaraEvent{{
		EventName:      "setCommanderCredits",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func mapFSDJump(ts string, p map[string]any, st *state) []inaraEvent {
	system, ok := str(p, "StarSystem")
	if !ok {
		return nil
	}
	st.lastSystem = system

	data := map[string]any{"starsystemName": system}
	if dist, ok := num(p, "JumpDist"); ok {
		data["jumpDistance"] = dist
	}
	if coords, ok := p["StarPos"].([]any); ok && len(coords) == 3 {
		data["starsystemCoords"] = coords
	}
	return []inaraEvent{{
		EventName:      "addCommanderTravelFSDJump",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func mapCarrierJump(ts string, p map[string]any, st *state) []inaraEvent {
	system, ok := str(p, "StarSystem")
	if !ok {
		return nil
	}
	st.lastSystem = system

	data := map[string]any{"starsystemName": system}
	if station, ok := str(p, "StationName"); ok {
		data["stationName"] = station
	}
	if marketID, ok := num(p, "MarketID"); ok {
		data["marketID"] = int64(marketID)
	}
	return []inaraEvent{{
		EventName:      "addCommanderTravelCarrierJump",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func mapDocked(ts string, p map[string]any, st *state) []inaraEvent {
	system, okSystem := str(p, "StarSystem")
	station, okStation := str(p, "StationName")
	if !okSystem || !okStation {
		return nil
	}
	st.lastSystem = system

	data := map[string]any{
		"starsystemName": system,
		"stationName":    station,
	}
	if marketID, ok := num(p, "MarketID"); ok {
		data["marketID"] = int64(marketID)
	}
	return []inaraEvent{{
		EventName:      "addCommanderTravelDock",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func mapLocation(ts string, p map[string]any, st *state) []inaraEvent {
	system, ok := str(p, "StarSystem")
	if !ok {
		return nil
	}
	st.lastSystem = system

	data := map[string]any{"starsystemName": system}
	if station, ok := str(p, "StationName"); ok {
		data["stationName"] = station
	}
	if marketID, ok := num(p, "MarketID"); ok {
		data["marketID"] = int64(marketID)
	}
	return []inaraEvent{{
		EventName:      "setCommanderTravelLocation",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

// mapRank は Rank イベント(各ランクの段位)を記録し、進捗が既知のものだけ送る。
//
// INARA は段位(rankValue)と段位内の進捗(rankProgress, 0..1)を一緒に
// 受け取る。進捗は Journal では別イベント(Progress)で来るため、進捗を
// まだ見ていない段階で送ると INARA 側の進捗が 0 に上書きされてしまう。
// Journal では Rank の直後に必ず Progress が来る(起動時も昇格時も)ので、
// 進捗が未知のうちは送らず Progress に任せる。
func mapRank(ts string, p map[string]any, st *state) []inaraEvent {
	var events []inaraEvent
	for journalKey, inaraName := range rankKeys {
		value, ok := num(p, journalKey)
		if !ok {
			continue
		}
		st.lastRanks[journalKey] = int(value)

		progress, seen := st.lastProg[journalKey]
		if !seen {
			continue
		}
		events = append(events, rankEvent(ts, inaraName, int(value), progress))
	}
	return events
}

// mapProgress は Progress イベント(段位内の進捗率)を送る。
// 段位そのものは直近の Rank イベント由来の値を使う。
func mapProgress(ts string, p map[string]any, st *state) []inaraEvent {
	var events []inaraEvent
	for journalKey, inaraName := range rankKeys {
		percent, ok := num(p, journalKey)
		if !ok {
			continue
		}
		progress := percent / 100
		st.lastProg[journalKey] = progress

		rank, seen := st.lastRanks[journalKey]
		if !seen {
			// 段位が分からないまま進捗だけ送ると INARA 側で段位が
			// 0 に落ちうるので、Rank を見るまで送らない。
			continue
		}
		events = append(events, rankEvent(ts, inaraName, rank, progress))
	}
	return events
}

func rankEvent(ts, rankName string, value int, progress float64) inaraEvent {
	return inaraEvent{
		EventName:      "setCommanderRankPilot",
		EventTimestamp: ts,
		EventData: map[string]any{
			"rankName":     rankName,
			"rankValue":    value,
			"rankProgress": progress,
		},
	}
}

// mapReputation は主要勢力への評判を送る。Journal は -100..100 の
// パーセント、INARA は -1..1 の比率。
func mapReputation(ts string, p map[string]any) []inaraEvent {
	var events []inaraEvent
	for _, faction := range majorFactions {
		percent, ok := num(p, faction)
		if !ok {
			continue
		}
		events = append(events, inaraEvent{
			EventName:      "setCommanderReputationMajorFaction",
			EventTimestamp: ts,
			EventData: map[string]any{
				"majorfactionName":       lower(faction),
				"majorfactionReputation": percent / 100,
			},
		})
	}
	return events
}

func mapEngineerProgress(ts string, p map[string]any) []inaraEvent {
	// 起動直後は全エンジニアの配列、以降は単体イベントで来る。
	if list, ok := p["Engineers"].([]any); ok {
		var events []inaraEvent
		for _, item := range list {
			entry, ok := item.(map[string]any)
			if !ok {
				continue
			}
			if ev, ok := engineerEvent(ts, entry); ok {
				events = append(events, ev)
			}
		}
		return events
	}

	if ev, ok := engineerEvent(ts, p); ok {
		return []inaraEvent{ev}
	}
	return nil
}

func engineerEvent(ts string, entry map[string]any) (inaraEvent, bool) {
	name, ok := str(entry, "Engineer")
	if !ok {
		return inaraEvent{}, false
	}
	data := map[string]any{"engineerName": name}
	if stage, ok := str(entry, "Progress"); ok {
		data["rankStage"] = stage
	}
	if rank, ok := num(entry, "Rank"); ok {
		data["rankValue"] = int(rank)
	}
	return inaraEvent{
		EventName:      "setCommanderRankEngineer",
		EventTimestamp: ts,
		EventData:      data,
	}, true
}

// mapMaterials は素材在庫を 1 つの INARA イベントにまとめる。
// Journal は Raw / Manufactured / Encoded に分かれているが、INARA は
// 種別を区別しない 1 本のリストを受け取る。
func mapMaterials(ts string, p map[string]any) []inaraEvent {
	var items []map[string]any
	for _, category := range []string{"Raw", "Manufactured", "Encoded"} {
		list, ok := p[category].([]any)
		if !ok {
			continue
		}
		for _, item := range list {
			entry, ok := item.(map[string]any)
			if !ok {
				continue
			}
			name, okName := str(entry, "Name")
			count, okCount := num(entry, "Count")
			if !okName || !okCount {
				continue
			}
			items = append(items, map[string]any{
				"itemName":  name,
				"itemCount": int64(count),
			})
		}
	}
	if len(items) == 0 {
		return nil
	}
	return []inaraEvent{{
		EventName:      "setCommanderInventoryMaterials",
		EventTimestamp: ts,
		EventData:      items,
	}}
}

// mapStatistics は Journal の Statistics をそのまま送る(INARA 側が
// Journal と同じ構造を受け付ける)。イベント本体のメタデータは落とす。
func mapStatistics(ts string, p map[string]any) []inaraEvent {
	data := map[string]any{}
	for key, value := range p {
		if key == "timestamp" || key == "event" {
			continue
		}
		data[key] = value
	}
	if len(data) == 0 {
		return nil
	}
	return []inaraEvent{{
		EventName:      "setCommanderGameStatistics",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func mapDied(ts string, p map[string]any, st *state) []inaraEvent {
	data := map[string]any{}
	if st.lastSystem != "" {
		data["starsystemName"] = st.lastSystem
	}
	if killer, ok := str(p, "KillerName"); ok {
		data["opponentName"] = killer
	} else if killers, ok := p["Killers"].([]any); ok && len(killers) > 0 {
		if first, ok := killers[0].(map[string]any); ok {
			if name, ok := str(first, "Name"); ok {
				data["opponentName"] = name
			}
		}
	}
	return []inaraEvent{{
		EventName:      "addCommanderCombatDeath",
		EventTimestamp: ts,
		EventData:      data,
	}}
}

func str(p map[string]any, key string) (string, bool) {
	v, ok := p[key].(string)
	return v, ok && v != ""
}

func num(p map[string]any, key string) (float64, bool) {
	v, ok := p[key].(float64)
	return v, ok
}

func lower(s string) string {
	out := []rune(s)
	for i, r := range out {
		if r >= 'A' && r <= 'Z' {
			out[i] = r + ('a' - 'A')
		}
	}
	return string(out)
}
