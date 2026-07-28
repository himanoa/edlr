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
