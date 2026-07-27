// Package inara は INARA API v1 のリクエスト組み立てと応答の解釈を持つ。
//
// ホストの import には依存しない。送信そのもの(driver-http の呼び出し)は
// main が行い、このパッケージは「何を送るか」と「返ってきたものをどう読むか」
// だけを扱う。
package inara

import "encoding/json"

// Event は INARA API v1 の 1 イベント。
//
// Timestamp は mapping が Journal イベントの timestamp で埋める。個々の
// マッパーに timestamp を配って回らずに済ませるため、New では設定しない。
type Event struct {
	Name      string `json:"eventName"`
	Timestamp string `json:"eventTimestamp"`
	Data      any    `json:"eventData"`
}

// New は timestamp 未設定の Event を作る。
func New(name string, data any) Event {
	return Event{Name: name, Data: data}
}

// Header は INARA がリクエストごとに要求する識別情報。
type Header struct {
	AppName          string `json:"appName"`
	AppVersion       string `json:"appVersion"`
	IsBeingDeveloped bool   `json:"isBeingDeveloped"`
	APIKey           string `json:"APIkey"`
	CommanderName    string `json:"commanderName"`
	FrontierID       string `json:"commanderFrontierID,omitempty"`
}

type request struct {
	Header Header  `json:"header"`
	Events []Event `json:"events"`
}

// Encode は送信する JSON を組み立てる。
func Encode(h Header, events []Event) ([]byte, error) {
	return json.Marshal(request{Header: h, Events: events})
}
