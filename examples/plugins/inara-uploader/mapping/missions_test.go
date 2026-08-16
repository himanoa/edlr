package mapping

import (
	"encoding/json"
	"testing"
)

func TestMissionAcceptedCarriesTheOriginFromTheCurrentLocation(t *testing.T) {
	st := newLiveState()
	st.LastSystem = "Sol"
	st.LastStation = "Abraham Lincoln"
	res := convertOne(t, st, "MissionAccepted", `{
		"Faction":"Sol Workers' Party","Name":"Mission_Delivery","MissionID":42,
		"Commodity":"$Gold_Name;","Count":10,
		"DestinationSystem":"Lave","DestinationStation":"Lave Station","TargetFaction":"Lave Crew",
		"Expiry":"2026-08-20T00:00:00Z","Influence":"+","Reputation":"++"}`)
	if len(res.Events) != 1 || res.Events[0].Name != "addCommanderMission" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `{"missionName":"Mission_Delivery","missionGameID":42,"missionExpiry":"2026-08-20T00:00:00Z",` +
		`"influenceGain":"+","reputationGain":"++",` +
		`"starsystemNameOrigin":"Sol","stationNameOrigin":"Abraham Lincoln","minorfactionNameOrigin":"Sol Workers' Party",` +
		`"starsystemNameTarget":"Lave","stationNameTarget":"Lave Station","minorfactionNameTarget":"Lave Crew",` +
		`"commodityName":"$Gold_Name;","commodityCount":10}`
	if string(body) != want {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestMissionCompletedAlsoAwardsPermits(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "MissionCompleted", `{
		"Name":"Mission_Delivery","MissionID":42,"Reward":100000,"Donated":0,
		"PermitsAwarded":["Sol","Founders World"],
		"CommodityReward":[{"Name":"Gold","Count":3}],
		"MaterialsReward":[{"Name":"iron","Category":"Raw","Count":5}]}`)
	if len(res.Events) != 3 {
		t.Fatalf("expected completion + 2 permits, got %+v", res.Events)
	}
	if res.Events[0].Name != "setCommanderMissionCompleted" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	want := `{"missionGameID":42,"donationCredits":0,"rewardCredits":100000,` +
		`"rewardPermits":["Sol","Founders World"],` +
		`"rewardCommodities":[{"itemName":"Gold","itemCount":3}],` +
		`"rewardMaterials":[{"itemName":"iron","itemCount":5}]}`
	if string(body) != want {
		t.Errorf("unexpected payload: %s", body)
	}
	if res.Events[1].Name != "addCommanderPermit" || res.Events[2].Name != "addCommanderPermit" {
		t.Fatalf("permits must be announced separately: %+v", res.Events)
	}
	body, _ = json.Marshal(res.Events[1].Data)
	if string(body) != `{"starsystemName":"Sol"}` {
		t.Errorf("unexpected permit payload: %s", body)
	}
}

func TestMissionFailureAndAbandonment(t *testing.T) {
	for event, want := range map[string]string{
		"MissionFailed":    "setCommanderMissionFailed",
		"MissionAbandoned": "setCommanderMissionAbandoned",
	} {
		st := newLiveState()
		res := convertOne(t, st, event, `{"Name":"Mission_Delivery","MissionID":42}`)
		if len(res.Events) != 1 || res.Events[0].Name != want {
			t.Fatalf("%s: unexpected events: %+v", event, res.Events)
		}
		body, _ := json.Marshal(res.Events[0].Data)
		if string(body) != `{"missionGameID":42}` {
			t.Errorf("%s: unexpected payload: %s", event, body)
		}
	}
}

func TestMissionsWithoutAnIDSendNothing(t *testing.T) {
	for _, event := range []string{"MissionAccepted", "MissionCompleted", "MissionFailed", "MissionAbandoned"} {
		st := newLiveState()
		if res := convertOne(t, st, event, `{"Name":"Mission_Delivery"}`); len(res.Events) != 0 {
			t.Errorf("%s without a MissionID must send nothing, got %+v", event, res.Events)
		}
	}
}
