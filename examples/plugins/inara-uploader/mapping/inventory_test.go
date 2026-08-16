package mapping

import (
	"encoding/json"
	"testing"
)

func TestMaterialsAreFlattenedIntoOneList(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Materials", `{"Raw":[{"Name":"iron","Count":10}],"Manufactured":[{"Name":"basicconductors","Count":3}],"Encoded":[]}`)

	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderInventoryMaterials" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `[{"itemName":"iron","itemCount":10},{"itemName":"basicconductors","itemCount":3}]` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestEmptyMaterialsSendNothing(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Materials", `{"Raw":[],"Manufactured":[],"Encoded":[]}`); len(res.Events) != 0 {
		t.Errorf("expected no events, got %+v", res.Events)
	}
}

func TestStatisticsDropEventMetadata(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Statistics", `{"timestamp":"2026-07-27T00:00:00Z","event":"Statistics","Bank_Account":{"Current_Wealth":1}}`)

	if len(res.Events) != 1 {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"Bank_Account":{"Current_Wealth":1}}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestStatisticsWithOnlyMetadataSendNothing(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Statistics", `{"timestamp":"t","event":"Statistics"}`); len(res.Events) != 0 {
		t.Errorf("expected no events, got %+v", res.Events)
	}
}

// Cargo は Inventory つきのときだけ全量を送る(以降の Cargo は Cargo.json への
// 参照だけでイベント本体に在庫が入っていない)。
func TestCargoSendsTheFullInventoryWhenPresent(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Cargo", `{"Vessel":"Ship","Count":13,
		"Inventory":[
			{"Name":"gold","Count":10},
			{"Name":"biowaste","Count":2,"Stolen":1,"MissionID":42},
			{"Name":"","Count":1}]}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderInventoryCargo" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `[{"itemName":"gold","itemCount":10},` +
		`{"itemName":"biowaste","itemCount":2,"isStolen":true,"missionGameID":42}]`
	if string(body) != want {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestCargoWithoutAnInventorySendsNothing(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Cargo", `{"Vessel":"Ship","Count":13}`); len(res.Events) != 0 {
		t.Errorf("Cargo without an inventory must send nothing, got %+v", res.Events)
	}
}
