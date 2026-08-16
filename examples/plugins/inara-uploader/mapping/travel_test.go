package mapping

import (
	"encoding/json"
	"testing"
)

func TestFSDJumpCarriesDistanceAndCoords(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "FSDJump", `{"StarSystem":"Sol","JumpDist":8.5,"StarPos":[1,2,3]}`)

	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderTravelFSDJump" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","jumpDistance":8.5,"starsystemCoords":[1,2,3]}` {
		t.Errorf("unexpected payload: %s", body)
	}
	if st.LastSystem != "Sol" {
		t.Errorf("FSDJump must record the system, got %q", st.LastSystem)
	}
}

func TestDockedNeedsBothSystemAndStation(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Docked", `{"StarSystem":"Sol"}`); len(res.Events) != 0 {
		t.Errorf("Docked without a station must send nothing, got %+v", res.Events)
	}
	if res := convertOne(t, st, "Docked", `{"StationName":"Abraham Lincoln"}`); len(res.Events) != 0 {
		t.Errorf("Docked without a system must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Docked", `{"StarSystem":"Sol","StationName":"Abraham Lincoln","MarketID":128}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderTravelDock" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","stationName":"Abraham Lincoln","marketID":128}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// Docked はステーションを覚え、FSDJump は離れたので忘れる。ミッション受注地や
// 船の輸送先など、Journal がステーション名を含まないイベントに添えるため。
func TestStationIsRememberedWhileDocked(t *testing.T) {
	st := newLiveState()
	convertOne(t, st, "Docked", `{"StarSystem":"Sol","StationName":"Abraham Lincoln"}`)
	if st.LastStation != "Abraham Lincoln" {
		t.Errorf("Docked must record the station, got %q", st.LastStation)
	}
	convertOne(t, st, "FSDJump", `{"StarSystem":"Alpha Centauri"}`)
	if st.LastStation != "" {
		t.Errorf("FSDJump must clear the station, got %q", st.LastStation)
	}
}

func TestTouchdownNeedsTheCurrentShip(t *testing.T) {
	st := newLiveState()
	st.LastSystem = "Sol"
	// INARA は着陸に船種を必須にしている。Loadout をまだ見ていなければ送らない。
	if res := convertOne(t, st, "Touchdown", `{"Body":"Sol 4"}`); len(res.Events) != 0 {
		t.Errorf("Touchdown without a known ship must send nothing, got %+v", res.Events)
	}

	st.ShipType = "Krait_MkII"
	st.ShipID = ptr(int64(3))
	res := convertOne(t, st, "Touchdown",
		`{"StarSystem":"Sol","Body":"Sol 4","PlayerControlled":true,"Taxi":false}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderTravelLand" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"starsystemName":"Sol","starsystemBodyName":"Sol 4","shipType":"Krait_MkII","shipGameID":3}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestTouchdownWithoutABodySendsNothing(t *testing.T) {
	st := newLiveState()
	st.ShipType = "Krait_MkII"
	st.ShipID = ptr(int64(3))
	st.LastSystem = "Sol"
	// 古い Journal の Touchdown(離陸後の再着陸など)は Body を含まないことがある。
	if res := convertOne(t, st, "Touchdown", `{"PlayerControlled":true}`); len(res.Events) != 0 {
		t.Errorf("Touchdown without a body must send nothing, got %+v", res.Events)
	}
}

// FSDJump / Location の Factions からは少数勢力への評判も送る。
func TestFactionsCarryMinorFactionReputation(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "FSDJump",
		`{"StarSystem":"Sol","Factions":[{"Name":"Sol Workers' Party","MyReputation":75},{"Name":"Mother Gaia","MyReputation":0},{"Name":"NoRep"}]}`)
	if len(res.Events) != 3 {
		t.Fatalf("expected jump + 2 reputations, got %+v", res.Events)
	}
	if res.Events[1].Name != "setCommanderReputationMinorFaction" {
		t.Fatalf("unexpected event: %+v", res.Events[1])
	}
	body, _ := json.Marshal(res.Events[1].Data)
	if string(body) != `{"minorfactionName":"Sol Workers' Party","minorfactionReputation":0.75}` {
		t.Errorf("unexpected payload: %s", body)
	}
	// MyReputation:0 は有効値(中立)なので送る。フィールド自体が無ければ送らない。
	body, _ = json.Marshal(res.Events[2].Data)
	if string(body) != `{"minorfactionName":"Mother Gaia","minorfactionReputation":0}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestLocationAlsoCarriesFactions(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Location",
		`{"StarSystem":"Sol","Factions":[{"Name":"Sol Workers' Party","MyReputation":-50}]}`)
	if len(res.Events) != 2 || res.Events[1].Name != "setCommanderReputationMinorFaction" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[1].Data)
	if string(body) != `{"minorfactionName":"Sol Workers' Party","minorfactionReputation":-0.5}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// Location と CarrierJump はステーションが無くても星系だけで送る。
func TestLocationAndCarrierJumpSendWithoutAStation(t *testing.T) {
	for name, want := range map[string]string{
		"Location":    "setCommanderTravelLocation",
		"CarrierJump": "addCommanderTravelCarrierJump",
	} {
		st := newLiveState()
		res := convertOne(t, st, name, `{"StarSystem":"Sol"}`)
		if len(res.Events) != 1 || res.Events[0].Name != want {
			t.Errorf("%s: unexpected events: %+v", name, res.Events)
		}
		if st.LastSystem != "Sol" {
			t.Errorf("%s must record the system", name)
		}
	}
}
