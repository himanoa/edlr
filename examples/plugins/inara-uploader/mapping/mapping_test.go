package mapping

import (
	"encoding/json"
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
	if got := err.Error(); got[:7] != "FSDJump" {
		t.Errorf("the error must name the event, got %q", got)
	}
}
