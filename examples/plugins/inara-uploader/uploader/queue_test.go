package uploader

import (
	"testing"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
)

func events(names ...string) []inara.Event {
	out := make([]inara.Event, 0, len(names))
	for _, name := range names {
		out = append(out, inara.New(name, nil))
	}
	return out
}

func names(evs []inara.Event) []string {
	out := make([]string, 0, len(evs))
	for _, ev := range evs {
		out = append(out, ev.Name)
	}
	return out
}

func TestQueueKeepsEverythingBelowTheLimit(t *testing.T) {
	q := newQueue(3)
	q.push(events("a", "b"))

	if q.len() != 2 {
		t.Errorf("expected 2 queued, got %d", q.len())
	}
	if q.takeDropped() != 0 {
		t.Error("nothing should have been dropped")
	}
}

// 上限を超えたら古いものから捨てる。INARA は現在の状態を反映するサービスなので、
// 古い travel ログを落として最新を残すほうが実害が小さい。
func TestQueueDropsTheOldestOverTheLimit(t *testing.T) {
	q := newQueue(3)
	q.push(events("a", "b", "c"))
	q.push(events("d", "e"))

	if got := names(q.peek()); len(got) != 3 || got[0] != "c" || got[2] != "e" {
		t.Errorf("expected the newest 3 (c d e), got %v", got)
	}
	if got := q.takeDropped(); got != 2 {
		t.Errorf("expected 2 dropped, got %d", got)
	}
}

// takeDropped は読み出したらリセットする(同じ破棄を二度報告しない)。
func TestTakeDroppedResets(t *testing.T) {
	q := newQueue(1)
	q.push(events("a", "b"))

	if got := q.takeDropped(); got != 1 {
		t.Fatalf("expected 1 dropped, got %d", got)
	}
	if got := q.takeDropped(); got != 0 {
		t.Errorf("dropped count must reset after being taken, got %d", got)
	}
}

// 1 回の push が上限より大きい場合も、最新のぶんだけ残る。
func TestQueueHandlesAPushLargerThanTheLimit(t *testing.T) {
	q := newQueue(2)
	q.push(events("a", "b", "c", "d"))

	if got := names(q.peek()); len(got) != 2 || got[0] != "c" || got[1] != "d" {
		t.Errorf("expected (c d), got %v", got)
	}
	if got := q.takeDropped(); got != 2 {
		t.Errorf("expected 2 dropped, got %d", got)
	}
}

func TestClearEmptiesTheQueue(t *testing.T) {
	q := newQueue(3)
	q.push(events("a"))
	q.clear()

	if q.len() != 0 || len(q.peek()) != 0 {
		t.Errorf("expected an empty queue, got %v", q.peek())
	}
}

// peek で受け取ったスライスを保持したまま push で上限超過を起こしても、
// 保持していたスライスの中身が変わらないことを検証する。
func TestPeekedSliceSurvivesALaterPush(t *testing.T) {
	q := newQueue(2)
	q.push(events("a", "b"))

	// peek で得たスライスを保持
	peeked := q.peek()
	if got := names(peeked); len(got) != 2 || got[0] != "a" || got[1] != "b" {
		t.Fatalf("initial peek should be (a b), got %v", got)
	}

	// 上限を超える push を実行
	q.push(events("c", "d"))

	// 保持していた peeked スライスの内容が変わっていないことを確認
	if got := names(peeked); len(got) != 2 || got[0] != "a" || got[1] != "b" {
		t.Errorf("peeked slice should remain (a b), got %v", got)
	}

	// 現在のキューの内容は新しいものに更新されている
	if got := names(q.peek()); len(got) != 2 || got[0] != "c" || got[1] != "d" {
		t.Errorf("queue should now contain (c d), got %v", got)
	}
}
