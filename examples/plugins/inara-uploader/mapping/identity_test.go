package mapping

import (
	"encoding/json"
	"testing"
)

// convertOne は 1 イベントを変換するテストヘルパー。
func convertOne(t *testing.T, st *State, name, payload string) Result {
	t.Helper()
	res, err := Convert(name, "2026-07-27T00:00:00Z", json.RawMessage(payload), st)
	if err != nil {
		t.Fatalf("Convert(%s) failed: %v", name, err)
	}
	return res
}

func TestCommanderLearnsIdentityWithoutSendingAnything(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "Commander", `{"Name":"Hutton","FID":"F123"}`)

	if len(res.Events) != 0 {
		t.Errorf("Commander must not produce inara events, got %v", res.Events)
	}
	if st.CommanderName != "Hutton" || st.FrontierID != "F123" {
		t.Errorf("identity was not learned: %+v", st)
	}
}

func TestLoadGameLearnsIdentityAndSendsCredits(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton","FID":"F123","Credits":1000,"Loan":0}`)

	if st.CommanderName != "Hutton" {
		t.Errorf("LoadGame must learn the commander name, got %q", st.CommanderName)
	}
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderCredits" {
		t.Fatalf("unexpected events: %+v", res.Events)
	}
	if res.Events[0].Timestamp != "2026-07-27T00:00:00Z" {
		t.Errorf("Convert must stamp the timestamp, got %q", res.Events[0].Timestamp)
	}

	body, _ := json.Marshal(res.Events[0].Data)
	// Loan は 0 でも送る(借金を返したことを INARA に反映させるため)。
	if string(body) != `{"commanderCredits":1000,"commanderLoan":0}` {
		t.Errorf("unexpected credits payload: %s", body)
	}
}

func TestLoadGameWithoutCreditsSendsNothing(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton"}`)
	if len(res.Events) != 0 {
		t.Errorf("expected no events without Credits, got %+v", res.Events)
	}
}

func TestLoanIsOmittedWhenAbsent(t *testing.T) {
	st := NewState()
	res := convertOne(t, st, "LoadGame", `{"Commander":"Hutton","Credits":5}`)
	body, _ := json.Marshal(res.Events[0].Data)
	if string(body) != `{"commanderCredits":5}` {
		t.Errorf("an absent loan must be omitted, got %s", body)
	}
}
