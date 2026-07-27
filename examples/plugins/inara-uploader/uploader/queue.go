package uploader

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// queue は送信待ちイベントを直近 max 件だけ保持するリングバッファ。
//
// 上限は「メモリ保護の保険」ではなく仕様。API キー未設定や capability 未承認で
// 送れない状態が続いてもメモリは伸びず、代わりに古いものから落ちる。
type queue struct {
	items   []inara.Event
	max     int
	dropped int
}

func newQueue(max int) *queue {
	return &queue{max: max}
}

func (q *queue) push(events []inara.Event) {
	q.items = append(q.items, events...)
	if overflow := len(q.items) - q.max; overflow > 0 {
		// peek が返したスライスを書き換えないよう、その場で詰めずに
		// 新しい配列へ写す。確保が起きるのは上限を超えたときだけ。
		kept := make([]inara.Event, q.max)
		copy(kept, q.items[overflow:])
		q.items = kept
		q.dropped += overflow
	}
}

func (q *queue) peek() []inara.Event { return q.items }

func (q *queue) len() int { return len(q.items) }

// clear は中身を捨てる。peek で借りたスライスを使い回さないよう、
// 同じ配列は再利用しない。
func (q *queue) clear() { q.items = nil }

// takeDropped は前回の報告以降に捨てた件数を返してリセットする。
func (q *queue) takeDropped() int {
	n := q.dropped
	q.dropped = 0
	return n
}
