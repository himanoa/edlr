// Package jumplog はプラグインの判断を持つ部分。
//
// `main` は `//go:wasmimport` を含むためネイティブでリンクできず、テストが
// 書けない。ホスト境界に触らないロジックはこちらへ寄せておくと
// `go test ./...` で確かめられる。
package jumplog

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Settings は manifest の `[[settings]]` に対応する。
type Settings struct {
	Enabled     bool
	MinDistance float64
}

// ParseSettings は host-settings.get-all() の JSON を解釈する。
// 壊れた JSON や未設定のキーは既定値へ倒す(プラグインを止めない)。
func ParseSettings(raw string) Settings {
	var v struct {
		Enabled     *bool    `json:"enabled"`
		MinDistance *float64 `json:"minDistance"`
	}
	s := Settings{Enabled: true, MinDistance: 0}
	if err := json.Unmarshal([]byte(raw), &v); err != nil {
		return s
	}
	if v.Enabled != nil {
		s.Enabled = *v.Enabled
	}
	if v.MinDistance != nil {
		s.MinDistance = *v.MinDistance
	}
	return s
}

// Jump は FSDJump から取り出した必要な部分。
type Jump struct {
	System   string
	Distance float64
}

// ParseJump は FSDJump の payload-json から星系名と跳躍距離を取り出す。
// 星系名が無いものは扱えないので false を返す。
func ParseJump(payloadJSON string) (Jump, bool) {
	var v struct {
		StarSystem string  `json:"StarSystem"`
		JumpDist   float64 `json:"JumpDist"`
	}
	if err := json.Unmarshal([]byte(payloadJSON), &v); err != nil {
		return Jump{}, false
	}
	if v.StarSystem == "" {
		return Jump{}, false
	}
	return Jump{System: v.StarSystem, Distance: v.JumpDist}, true
}

// Queue は未処理のジャンプを保持する。EDSM への問い合わせは
// `on-schedule` 1 回につき 1 件しか流さないので、際限なく溜めない。
type Queue struct {
	capacity int
	items    []Jump
}

func NewQueue(capacity int) *Queue {
	return &Queue{capacity: capacity}
}

// Push は末尾へ積む。上限を超えたら古いものから捨てる。
func (q *Queue) Push(j Jump) {
	if len(q.items) >= q.capacity {
		q.items = q.items[1:]
	}
	q.items = append(q.items, j)
}

// Pop は先頭を 1 件取り出す。空なら false。
func (q *Queue) Pop() (Jump, bool) {
	if len(q.items) == 0 {
		return Jump{}, false
	}
	j := q.items[0]
	q.items = q.items[1:]
	return j, true
}

func (q *Queue) Len() int { return len(q.items) }

// Summary は残っているぶんを 1 行に畳む(on-stop のログ用)。
func (q *Queue) Summary() string {
	parts := make([]string, 0, len(q.items))
	for _, j := range q.items {
		parts = append(parts, fmt.Sprintf("%s (%.2f ly)", j.System, j.Distance))
	}
	return strings.Join(parts, ", ")
}

// EDSMURL は問い合わせ先を組み立てる。星系名にはスペースやハイフンが入る
// (`Col 285 Sector AA-A a1`)ので、依存を増やさない範囲でエスケープする。
func EDSMURL(endpoint, system string) string {
	return endpoint + "?systemName=" + urlEncode(system) + "&showId=1"
}

func urlEncode(s string) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'A' && c <= 'Z', c >= 'a' && c <= 'z', c >= '0' && c <= '9',
			c == '-', c == '_', c == '.', c == '~':
			b.WriteByte(c)
		default:
			fmt.Fprintf(&b, "%%%02X", c)
		}
	}
	return b.String()
}
