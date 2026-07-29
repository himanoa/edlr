package jumplog

import "testing"

func TestParseSettingsDefaults(t *testing.T) {
	s := ParseSettings("{}")
	if !s.Enabled || s.MinDistance != 0 {
		t.Fatalf("unexpected defaults: %+v", s)
	}
	// 壊れた JSON でも既定値へ倒れる(プラグインを止めない)
	if got := ParseSettings("not json"); !got.Enabled {
		t.Fatalf("broken settings should fall back to enabled: %+v", got)
	}
}

func TestParseSettingsValues(t *testing.T) {
	s := ParseSettings(`{"enabled":false,"minDistance":12.5}`)
	if s.Enabled {
		t.Fatal("enabled should be false")
	}
	if s.MinDistance != 12.5 {
		t.Fatalf("minDistance = %v", s.MinDistance)
	}
}

func TestParseJump(t *testing.T) {
	j, ok := ParseJump(`{"event":"FSDJump","StarSystem":"Sol","JumpDist":8.19}`)
	if !ok || j.System != "Sol" || j.Distance != 8.19 {
		t.Fatalf("got %+v ok=%v", j, ok)
	}
	if _, ok := ParseJump(`{"event":"FSDJump"}`); ok {
		t.Fatal("a payload without StarSystem should be rejected")
	}
	if _, ok := ParseJump("{"); ok {
		t.Fatal("broken JSON should be rejected")
	}
}

func TestQueueDropsOldest(t *testing.T) {
	q := NewQueue(2)
	q.Push(Jump{System: "A"})
	q.Push(Jump{System: "B"})
	q.Push(Jump{System: "C"})
	if q.Len() != 2 {
		t.Fatalf("len = %d", q.Len())
	}
	if j, _ := q.Pop(); j.System != "B" {
		t.Fatalf("oldest should have been dropped, got %s", j.System)
	}
	if j, _ := q.Pop(); j.System != "C" {
		t.Fatalf("got %s", j.System)
	}
	if _, ok := q.Pop(); ok {
		t.Fatal("queue should be empty")
	}
}

func TestEDSMURLEscapes(t *testing.T) {
	got := EDSMURL("https://www.edsm.net/api-v1/system", "Col 285 Sector AA-A a1")
	want := "https://www.edsm.net/api-v1/system?systemName=Col%20285%20Sector%20AA-A%20a1&showId=1"
	if got != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}
