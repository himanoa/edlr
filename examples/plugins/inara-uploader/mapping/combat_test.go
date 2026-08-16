package mapping

import (
	"encoding/json"
	"testing"
)

func TestDiedBorrowsTheSystemFromTheLastTravelEvent(t *testing.T) {
	st := newLiveState()
	convertOne(t, st, "FSDJump", `{"StarSystem":"Sol"}`)
	res := convertOne(t, st, "Died", `{"KillerName":"Salvation"}`)

	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","opponentName":"Salvation"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestDiedFallsBackToTheFirstOfAWing(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Died", `{"Killers":[{"Name":"Wing Leader"},{"Name":"Wingman"}]}`)

	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"opponentName":"Wing Leader"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestPVPKillNeedsAKnownSystem(t *testing.T) {
	st := newLiveState()
	// INARA は starsystemName を必須にしているので、星系が未学習なら送らない。
	if res := convertOne(t, st, "PVPKill", `{"Victim":"Rival"}`); len(res.Events) != 0 {
		t.Errorf("PVPKill without a known system must send nothing, got %+v", res.Events)
	}

	st.LastSystem = "Sol"
	res := convertOne(t, st, "PVPKill", `{"Victim":"Rival","CombatRank":8}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderCombatKill" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","opponentName":"Rival"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestInterdictionVariants(t *testing.T) {
	for _, tc := range []struct {
		event   string
		payload string
		name    string
		want    string
	}{
		{
			"Interdiction",
			`{"Success":true,"IsPlayer":true,"Interdicted":"Prey"}`,
			"addCommanderCombatInterdiction",
			`{"starsystemName":"Sol","opponentName":"Prey","isPlayer":true,"isSuccess":true}`,
		},
		{
			// NPC 相手は Interdicted が空で Power / Faction に名前が入ることがある。
			"Interdiction",
			`{"Success":false,"IsPlayer":false,"Power":"Zemina Torval"}`,
			"addCommanderCombatInterdiction",
			`{"starsystemName":"Sol","opponentName":"Zemina Torval","isPlayer":false,"isSuccess":false}`,
		},
		{
			"Interdicted",
			`{"Submitted":true,"Interdictor":"Pirate","IsPlayer":false}`,
			"addCommanderCombatInterdicted",
			`{"starsystemName":"Sol","opponentName":"Pirate","isPlayer":false,"isSubmit":true}`,
		},
		{
			"EscapeInterdiction",
			`{"Interdictor":"Pirate","IsPlayer":true}`,
			"addCommanderCombatInterdictionEscape",
			`{"starsystemName":"Sol","opponentName":"Pirate","isPlayer":true}`,
		},
	} {
		st := newLiveState()
		st.LastSystem = "Sol"
		res := convertOne(t, st, tc.event, tc.payload)
		if len(res.Events) != 1 || res.Events[0].Name != tc.name {
			t.Fatalf("%s: unexpected events: %+v", tc.event, res.Events)
		}
		body, _ := json.Marshal(res.Events[0].Data)
		if string(body) != tc.want {
			t.Errorf("%s: unexpected payload: %s", tc.event, body)
		}
	}
}

func TestInterdictionEventsNeedAKnownSystem(t *testing.T) {
	for _, event := range []string{"Interdiction", "Interdicted", "EscapeInterdiction"} {
		st := newLiveState()
		if res := convertOne(t, st, event, `{"Interdictor":"Pirate","Interdicted":"Prey"}`); len(res.Events) != 0 {
			t.Errorf("%s without a known system must send nothing, got %+v", event, res.Events)
		}
	}
}

func TestShutdownOnlyRequestsAFlush(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Shutdown", `{}`)
	if len(res.Events) != 0 {
		t.Errorf("Shutdown must not produce events, got %+v", res.Events)
	}
	if !res.FlushLive {
		t.Error("Shutdown must request a flush")
	}
}
