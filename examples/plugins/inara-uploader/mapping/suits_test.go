package mapping

import (
	"encoding/json"
	"testing"
)

func TestSuitLoadoutVariantsSendTheFullLoadout(t *testing.T) {
	payload := `{
		"SuitID":100,"SuitName":"tacticalsuit_class3","SuitMods":["suit_increasedsprintduration"],
		"LoadoutID":4293000001,"LoadoutName":"Assault",
		"Modules":[{"SlotName":"PrimaryWeapon1","SuitModuleID":200,
			"ModuleName":"wpn_m_assaultrifle_kinetic_fauto","Class":2,
			"WeaponMods":["weapon_stability"]}]}`
	want := `{"loadoutGameID":4293000001,"suitType":"tacticalsuit_class3","suitGameID":100,` +
		`"loadoutName":"Assault","suitMods":["suit_increasedsprintduration"],` +
		`"suitLoadout":[{"slotName":"PrimaryWeapon1","itemName":"wpn_m_assaultrifle_kinetic_fauto",` +
		`"itemGameID":200,"itemClass":2,"itemMods":["weapon_stability"]}]}`

	for _, event := range []string{"SuitLoadout", "SwitchSuitLoadout", "CreateSuitLoadout"} {
		st := newLiveState()
		res := convertOne(t, st, event, payload)
		if len(res.Events) != 1 || res.Events[0].Name != "setCommanderSuitLoadout" {
			t.Fatalf("%s: unexpected events: %+v", event, res.Events)
		}
		body, _ := json.Marshal(res.Events[0].Data)
		if string(body) != want {
			t.Errorf("%s: unexpected payload: %s", event, body)
		}
	}
}

func TestRenameSuitLoadoutOnlyUpdatesTheName(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "RenameSuitLoadout",
		`{"SuitID":100,"SuitName":"tacticalsuit_class3","LoadoutID":4293000001,"LoadoutName":"Recon"}`)
	if len(res.Events) != 1 || res.Events[0].Name != "updateCommanderSuitLoadout" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"loadoutGameID":4293000001,"suitType":"tacticalsuit_class3","suitGameID":100,"loadoutName":"Recon"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestDeleteSuitLoadout(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "DeleteSuitLoadout", `{"LoadoutID":4293000001}`)
	if len(res.Events) != 1 || res.Events[0].Name != "delCommanderSuitLoadout" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"loadoutGameID":4293000001}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestCommunityGoalSendsGoalAndOwnProgress(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "CommunityGoal", `{"CurrentGoals":[{
		"CGID":726,"Title":"Alliance Research Initiative","SystemName":"Kaushpoos",
		"MarketName":"Neville Horizons","Expiry":"2026-08-20T00:00:00Z",
		"IsComplete":false,"CurrentTotal":10062,"PlayerContribution":4,
		"NumContributors":123,"TopRankSize":10,"PlayerInTopRank":false,
		"TierReached":"Tier 2","PlayerPercentileBand":50,"Bonus":200000}]}`)
	if len(res.Events) != 2 {
		t.Fatalf("expected goal + progress, got %+v", res.Events)
	}
	if res.Events[0].Name != "setCommunityGoal" || res.Events[1].Name != "setCommanderCommunityGoalProgress" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `{"communitygoalGameID":726,"communitygoalName":"Alliance Research Initiative",` +
		`"starsystemName":"Kaushpoos","stationName":"Neville Horizons","goalExpiry":"2026-08-20T00:00:00Z",` +
		`"tierReached":2,"topRankSize":10,"isCompleted":false,"contributorsNum":123,` +
		`"contributionsTotal":10062,"completionBonus":200000}`
	if string(body) != want {
		t.Errorf("unexpected goal payload: %s", body)
	}
	body, _ = json.Marshal(res.Events[1].Data)
	want = `{"communitygoalGameID":726,"contribution":4,"percentileBand":50,"isTopRank":false}`
	if string(body) != want {
		t.Errorf("unexpected progress payload: %s", body)
	}
}

func TestFriendsOnlyAnnounceAddedAndLost(t *testing.T) {
	st := newLiveState()
	for status, want := range map[string]string{
		"Added": "addCommanderFriend",
		"Lost":  "delCommanderFriend",
	} {
		res := convertOne(t, st, "Friends", `{"Status":"`+status+`","Name":"Jameson"}`)
		if len(res.Events) != 1 || res.Events[0].Name != want {
			t.Fatalf("Friends %s: unexpected events: %+v", status, res.Events)
		}
		body, _ := json.Marshal(res.Events[0].Data)
		if string(body) != `{"commanderName":"Jameson"}` {
			t.Errorf("Friends %s: unexpected payload: %s", status, body)
		}
	}
	// Online / Offline / Requested / Declined は友達リストの変化ではない。
	for _, status := range []string{"Online", "Offline", "Requested", "Declined"} {
		if res := convertOne(t, st, "Friends", `{"Status":"`+status+`","Name":"Jameson"}`); len(res.Events) != 0 {
			t.Errorf("Friends %s must send nothing, got %+v", status, res.Events)
		}
	}
}
