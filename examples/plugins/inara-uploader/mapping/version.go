package mapping

import (
	"strconv"
	"strings"
)

// isLiveVersion は gameversion が Live のゲーム(Odyssey / Horizons 4.0 以降)
// かどうかを判定する。INARA のルールは「Live のみ送信可、Legacy(3.8)と
// beta は送信禁止」なので、メジャーバージョン >= 4 かつ beta を含まない
// ことを条件にする。パース不能・空文字列は false(送らない側に倒す)。
func isLiveVersion(gameversion string) bool {
	if strings.Contains(strings.ToLower(gameversion), "beta") {
		return false
	}
	head := gameversion
	if i := strings.IndexByte(head, '.'); i >= 0 {
		head = head[:i]
	}
	major, err := strconv.Atoi(strings.TrimSpace(head))
	if err != nil {
		return false
	}
	return major >= 4
}
