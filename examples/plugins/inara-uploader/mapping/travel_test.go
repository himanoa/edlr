package mapping

import (
	"encoding/json"
	"testing"
)

func TestFSDJumpCarriesDistanceAndCoords(t *testing.T) {
	st := NewState()
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
	st := NewState()
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

// Location と CarrierJump はステーションが無くても星系だけで送る。
func TestLocationAndCarrierJumpSendWithoutAStation(t *testing.T) {
	for name, want := range map[string]string{
		"Location":    "setCommanderTravelLocation",
		"CarrierJump": "addCommanderTravelCarrierJump",
	} {
		st := NewState()
		res := convertOne(t, st, name, `{"StarSystem":"Sol"}`)
		if len(res.Events) != 1 || res.Events[0].Name != want {
			t.Errorf("%s: unexpected events: %+v", name, res.Events)
		}
		if st.LastSystem != "Sol" {
			t.Errorf("%s must record the system", name)
		}
	}
}
