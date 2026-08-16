package mapping

import (
	"encoding/json"
	"testing"
)

func TestLoadoutSendsShipAndLoadoutAndLearnsTheCurrentShip(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Loadout", `{
		"Ship":"Krait_MkII","ShipID":3,"ShipName":"Nostromo","ShipIdent":"HI-88",
		"HullValue":1000,"ModulesValue":2000,"Rebuy":150,
		"MaxJumpRange":55.5,"CargoCapacity":64,"Hot":false,
		"Modules":[{"Slot":"MediumHardpoint1","Item":"hpt_multicannon_gimbal_medium","On":true,
			"Priority":0,"Health":0.95,"Value":38000,"AmmoInClip":90,"AmmoInHopper":2100,
			"Engineering":{"BlueprintName":"Weapon_Overcharged","Level":5,"Quality":0.9,
				"ExperimentalEffect_Localised":"Autoloader",
				"Modifiers":[{"Label":"Damage","Value":6.5,"OriginalValue":4.6,"LessIsGood":0}]}}]}`)

	if len(res.Events) != 2 {
		t.Fatalf("expected ship + loadout, got %+v", res.Events)
	}
	if res.Events[0].Name != "setCommanderShip" || res.Events[1].Name != "setCommanderShipLoadout" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `{"shipType":"Krait_MkII","shipGameID":3,"shipName":"Nostromo","shipIdent":"HI-88",` +
		`"isCurrentShip":true,"shipHullValue":1000,"shipModulesValue":2000,"shipRebuyCost":150,` +
		`"shipMaxJumpRange":55.5,"shipCargoCapacity":64}`
	if string(body) != want {
		t.Errorf("unexpected ship payload: %s", body)
	}
	body, _ = json.Marshal(res.Events[1].Data)
	want = `{"shipType":"Krait_MkII","shipGameID":3,"shipLoadout":[` +
		`{"slotName":"MediumHardpoint1","itemName":"hpt_multicannon_gimbal_medium","itemValue":38000,` +
		`"itemHealth":0.95,"isOn":true,"itemPriority":0,"itemAmmoClip":90,"itemAmmoHopper":2100,` +
		`"engineering":{"blueprintName":"Weapon_Overcharged","blueprintLevel":5,"blueprintQuality":0.9,` +
		`"experimentalEffect":"Autoloader","modifiers":[{"name":"Damage","value":6.5,"originalValue":4.6,"lessIsGood":false}]}}]}`
	if string(body) != want {
		t.Errorf("unexpected loadout payload: %s", body)
	}
	if st.ShipType != "Krait_MkII" || st.ShipID == nil || *st.ShipID != 3 {
		t.Errorf("Loadout must learn the current ship: %q %v", st.ShipType, st.ShipID)
	}
}

func TestShipyardLifecycle(t *testing.T) {
	st := newLiveState()

	res := convertOne(t, st, "ShipyardNew", `{"ShipType":"Anaconda","NewShipID":7}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderShip" {
		t.Fatalf("ShipyardNew: unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"Anaconda","shipGameID":7}` {
		t.Errorf("ShipyardNew: unexpected payload: %s", body)
	}
	if st.ShipType != "Anaconda" || st.ShipID == nil || *st.ShipID != 7 {
		t.Errorf("ShipyardNew must learn the current ship: %q %v", st.ShipType, st.ShipID)
	}

	res = convertOne(t, st, "ShipyardSwap", `{"ShipType":"Python","ShipID":2,"StoreOldShip":"Anaconda","StoreShipID":7}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderShip" {
		t.Fatalf("ShipyardSwap: unexpected events: %+v", res.Events)
	}
	body, _ = json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"Python","shipGameID":2,"isCurrentShip":true}` {
		t.Errorf("ShipyardSwap: unexpected payload: %s", body)
	}
	if st.ShipType != "Python" || st.ShipID == nil || *st.ShipID != 2 {
		t.Errorf("ShipyardSwap must learn the current ship: %q %v", st.ShipType, st.ShipID)
	}

	res = convertOne(t, st, "ShipyardSell", `{"ShipType":"Anaconda","SellShipID":7}`)
	if len(res.Events) != 1 || res.Events[0].Name != "delCommanderShip" {
		t.Fatalf("ShipyardSell: unexpected events: %+v", res.Events)
	}
	body, _ = json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"Anaconda","shipGameID":7}` {
		t.Errorf("ShipyardSell: unexpected payload: %s", body)
	}
}

func TestSetUserShipNameUpdatesTheShip(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "SetUserShipName",
		`{"Ship":"python","ShipID":2,"UserShipName":"Baba Yaga","UserShipId":"HI-77"}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderShip" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"python","shipGameID":2,"shipName":"Baba Yaga","shipIdent":"HI-77"}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// ShipyardTransfer は輸送先(= 今いる場所)を INARA が必須にしているため、
// ステーションが未学習なら送らない。
func TestShipyardTransferNeedsTheCurrentStation(t *testing.T) {
	st := newLiveState()
	payload := `{"ShipType":"Anaconda","ShipID":7,"System":"Sol","TransferTime":600}`
	if res := convertOne(t, st, "ShipyardTransfer", payload); len(res.Events) != 0 {
		t.Errorf("ShipyardTransfer without a known station must send nothing, got %+v", res.Events)
	}

	st.LastSystem = "Alpha Centauri"
	st.LastStation = "Hutton Orbital"
	res := convertOne(t, st, "ShipyardTransfer", payload)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderShipTransfer" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"Anaconda","shipGameID":7,"starsystemName":"Alpha Centauri","stationName":"Hutton Orbital","transferTime":600}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestStoredShipsListsEveryKnownShip(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "StoredShips", `{
		"StationName":"Jameson Memorial","StarSystem":"Shinrarta Dezhra","MarketID":128,
		"ShipsHere":[{"ShipID":5,"ShipType":"vulture","Name":"Vlad","Value":5000,"Hot":true}],
		"ShipsRemote":[{"ShipID":6,"ShipType":"dolphin","Value":1500,"StarSystem":"Sol"}]}`)
	if len(res.Events) != 2 {
		t.Fatalf("expected one event per stored ship, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"shipType":"vulture","shipGameID":5,"shipName":"Vlad","isHot":true,"starsystemName":"Shinrarta Dezhra","stationName":"Jameson Memorial","marketID":128}` {
		t.Errorf("unexpected payload for a local ship: %s", body)
	}
	body, _ = json.Marshal(res.Events[1].Data)
	if string(body) != `{"shipType":"dolphin","shipGameID":6,"starsystemName":"Sol"}` {
		t.Errorf("unexpected payload for a remote ship: %s", body)
	}
}

func TestStoredModulesSendsTheWholeStorage(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "StoredModules", `{
		"StarSystem":"Sol","StationName":"Abraham Lincoln","MarketID":128,
		"Items":[
			{"Name":"$int_fueltank_size4_class3_name;","StarSystem":"Sol","MarketID":128,"BuyPrice":24734,"Hot":false},
			{"Name":"$hpt_railgun_fixed_medium_name;","StarSystem":"Lave","MarketID":64,"BuyPrice":412800,"Hot":true,
				"EngineerModifications":"Weapon_LongRange","Level":4,"Quality":0.8},
			{"Name":"$int_cargorack_size6_class1_name;","BuyPrice":1000,"InTransit":true}]}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderStorageModules" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `[{"itemName":"$int_fueltank_size4_class3_name;","itemValue":24734,"starsystemName":"Sol","marketID":128},` +
		`{"itemName":"$hpt_railgun_fixed_medium_name;","itemValue":412800,"isHot":true,"starsystemName":"Lave","marketID":64,` +
		`"engineering":{"blueprintName":"Weapon_LongRange","blueprintLevel":4,"blueprintQuality":0.8}},` +
		`{"itemName":"$int_cargorack_size6_class1_name;","itemValue":1000}]`
	if string(body) != want {
		t.Errorf("unexpected payload: %s", body)
	}
}
