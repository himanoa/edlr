package mapping

import (
	"encoding/json"
	"testing"
)

// 段位だけ先に来ても送らない。送ると INARA 側の進捗が 0 に潰れる。
func TestRankWaitsForProgress(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Rank", `{"Combat":3}`); len(res.Events) != 0 {
		t.Fatalf("Rank alone must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Progress", `{"Combat":40}`)
	if len(res.Events) != 1 {
		t.Fatalf("Progress must send the pending rank, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"rankName":"combat","rankValue":3,"rankProgress":0.4}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// 進捗だけ先に来ても送らない。送ると INARA 側の段位が 0 に落ちうる。
func TestProgressWaitsForRank(t *testing.T) {
	st := newLiveState()
	if res := convertOne(t, st, "Progress", `{"Trade":10}`); len(res.Events) != 0 {
		t.Fatalf("Progress alone must send nothing, got %+v", res.Events)
	}

	res := convertOne(t, st, "Rank", `{"Trade":2}`)
	if len(res.Events) != 1 {
		t.Fatalf("Rank must send once progress is known, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"rankName":"trade","rankValue":2,"rankProgress":0.1}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// 送信順は固定(map を回すと毎回変わる)。
func TestRankOrderIsStable(t *testing.T) {
	st := newLiveState()
	convertOne(t, st, "Progress", `{"Combat":10,"Trade":20,"Explore":30}`)
	res := convertOne(t, st, "Rank", `{"Combat":1,"Trade":2,"Explore":3}`)

	var names []string
	for _, ev := range res.Events {
		names = append(names, ev.Data.(pilotRank).Name)
	}
	want := []string{"combat", "trade", "exploration"}
	if len(names) != len(want) {
		t.Fatalf("expected %v, got %v", want, names)
	}
	for i := range want {
		if names[i] != want[i] {
			t.Fatalf("expected %v, got %v", want, names)
		}
	}
}

func TestReputationIsScaledToRatio(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Reputation", `{"Empire":50,"Federation":-25}`)
	if len(res.Events) != 2 {
		t.Fatalf("expected 2 events, got %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events)
	want := `[{"eventName":"setCommanderReputationMajorFaction","eventTimestamp":"2026-07-27T00:00:00Z","eventData":{"majorfactionName":"empire","majorfactionReputation":0.5}},` +
		`{"eventName":"setCommanderReputationMajorFaction","eventTimestamp":"2026-07-27T00:00:00Z","eventData":{"majorfactionName":"federation","majorfactionReputation":-0.25}}]`
	if string(body) != want {
		t.Errorf("unexpected payload:\n got %s\nwant %s", body, want)
	}
}

func TestEngineerProgressAcceptsBothShapes(t *testing.T) {
	st := newLiveState()

	batch := convertOne(t, st, "EngineerProgress", `{"Engineers":[{"Engineer":"Felicity Farseer","Progress":"Unlocked","Rank":5},{"Engineer":"Elvira Martuuk","Progress":"Known"}]}`)
	if len(batch.Events) != 2 {
		t.Fatalf("expected 2 events from the array form, got %+v", batch.Events)
	}

	single := convertOne(t, st, "EngineerProgress", `{"Engineer":"Felicity Farseer","Progress":"Unlocked","Rank":5}`)
	if len(single.Events) != 1 {
		t.Fatalf("expected 1 event from the single form, got %+v", single.Events)
	}
	body, _ := json.Marshal(single.Events[0].Data)
	if string(body) != `{"engineerName":"Felicity Farseer","rankStage":"Unlocked","rankValue":5}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

func TestPowerplaySendsThePowerRank(t *testing.T) {
	st := newLiveState()
	res := convertOne(t, st, "Powerplay", `{"Power":"Zemina Torval","Rank":3,"Merits":880,"TimePledged":86400}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderRankPower" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"powerName":"Zemina Torval","rankValue":3,"meritsValue":880}` {
		t.Errorf("unexpected payload: %s", body)
	}
}

// Promotion は Rank と同じ形(昇進した分野だけが入る)。
func TestPromotionBehavesLikeRank(t *testing.T) {
	st := newLiveState()
	st.progress["combat"] = 0.1
	res := convertOne(t, st, "Promotion", `{"Combat":6}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderRankPilot" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	if st.ranks["combat"] != 6 {
		t.Errorf("Promotion must update the learned rank, got %v", st.ranks)
	}
}
