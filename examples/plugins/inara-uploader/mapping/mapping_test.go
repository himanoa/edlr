package mapping

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestUnknownEventsAreIgnored(t *testing.T) {
	st := NewState()
	// 未知のイベントはデコードもしないので、壊れた JSON でもエラーにならない。
	res, err := Convert("Scan", "2026-07-27T00:00:00Z", json.RawMessage("not json"), st)
	if err != nil {
		t.Fatalf("unknown events must not fail: %v", err)
	}
	if len(res.Events) != 0 || res.FlushLive {
		t.Errorf("unexpected result: %+v", res)
	}
}

func TestBrokenPayloadIsReportedWithTheEventName(t *testing.T) {
	st := NewState()
	_, err := Convert("FSDJump", "2026-07-27T00:00:00Z", json.RawMessage("{oops"), st)
	if err == nil {
		t.Fatal("expected an error for a broken payload")
	}
	if got := err.Error(); !strings.HasPrefix(got, "FSDJump:") {
		t.Errorf("the error must name the event, got %q", got)
	}
}

// --- Live 版ゲート ---

func convertOK(t *testing.T, st *State, name, payload string) Result {
	t.Helper()
	res, err := Convert(name, "2026-07-28T00:00:00Z", json.RawMessage(payload), st)
	if err != nil {
		t.Fatalf("%s: %v", name, err)
	}
	return res
}

func TestEventsBeforeAnyLoadGameAreDropped(t *testing.T) {
	st := NewState()
	res := convertOK(t, st, "FSDJump",
		`{"StarSystem":"Sol","SystemAddress":1,"JumpDist":8.1}`)
	if len(res.Events) != 0 {
		t.Errorf("events before the game version is known must be dropped, got %+v", res.Events)
	}
}

func TestLegacySessionsAreDroppedButStillLearnIdentity(t *testing.T) {
	st := NewState()
	res := convertOK(t, st, "LoadGame",
		`{"Commander":"Jameson","FID":"F123","Credits":1000,"gameversion":"3.8.0.404"}`)
	if len(res.Events) != 0 {
		t.Errorf("a legacy LoadGame must not emit events, got %+v", res.Events)
	}
	if st.CommanderName != "Jameson" || st.FrontierID != "F123" {
		t.Errorf("identity must still be learned in legacy sessions: %+v", st)
	}
	res = convertOK(t, st, "FSDJump",
		`{"StarSystem":"Sol","SystemAddress":1,"JumpDist":8.1}`)
	if len(res.Events) != 0 {
		t.Errorf("legacy session events must be dropped, got %+v", res.Events)
	}
}

func TestLiveSessionsAreConverted(t *testing.T) {
	st := NewState()
	res := convertOK(t, st, "LoadGame",
		`{"Commander":"Jameson","FID":"F123","Credits":1000,"gameversion":"4.0.0.1904"}`)
	if len(res.Events) != 1 || res.Events[0].Name != "setCommanderCredits" {
		t.Fatalf("a live LoadGame must emit credits, got %+v", res.Events)
	}
	res = convertOK(t, st, "FSDJump",
		`{"StarSystem":"Sol","SystemAddress":1,"JumpDist":8.1}`)
	if len(res.Events) == 0 {
		t.Error("live session events must be converted")
	}
}

func TestSwitchingBackToLiveReenablesUploads(t *testing.T) {
	st := NewState()
	convertOK(t, st, "LoadGame", `{"Commander":"J","gameversion":"3.8.0.404"}`)
	convertOK(t, st, "LoadGame", `{"Commander":"J","gameversion":"4.0.0.1904"}`)
	res := convertOK(t, st, "FSDJump",
		`{"StarSystem":"Sol","SystemAddress":1,"JumpDist":8.1}`)
	if len(res.Events) == 0 {
		t.Error("after a live LoadGame the gate must open again")
	}
}

func ptr[T any](v T) *T { return &v }

// newLiveState は Live 版ゲートが開いた状態の State を作る(ゲートの
// 挙動自体を検証しないテスト用)。
func newLiveState() *State {
	st := NewState()
	st.learnGameVersion("4.0.0.1904")
	return st
}
