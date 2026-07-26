// Package settings は manifest の `[[settings]]` に対応する設定値の解釈を持つ。
//
// ホストの import(wasm)に依存しない純粋なコードだけを置いてある。main
// パッケージは `//go:wasmimport` を含むため `go test` でリンクできず、
// ここに分けておかないと設定まわりのテストが書けない。
package settings

import "encoding/json"

// Settings は manifest の `[[settings]]` に対応する。値は毎イベント
// `host-settings.get-all` から読み直すので、UI での変更は次のイベントから効く。
type Settings struct {
	Enabled          bool
	APIKey           string
	CommanderName    string
	IsBeingDeveloped bool
	BatchSize        int
	MinIntervalSec   int
	UploadHistorical bool
	DryRun           bool
}

// Defaults は manifest の `default` と一致させること。
func Defaults() Settings {
	return Settings{
		Enabled:          true,
		IsBeingDeveloped: true,
		BatchSize:        10,
		MinIntervalSec:   60,
		// デーモンが動き出す前に書かれていたイベント(`event.replay`)も
		// 既定で送る。edlr が Journal の読み取り位置を永続化するように
		// なったため、再起動をまたいだ重複送信は起きない。既定で捨てると
		// デーモンが止まっていた間のイベントが恒久的に欠けるだけになる。
		UploadHistorical: true,
	}
}

// Parse は host-settings の JSON を Settings へ変換する。値が無い・型が違う・
// JSON が壊れている場合はいずれも既定値のまま(プラグインを止めない)。
func Parse(s string) Settings {
	cfg := Defaults()

	var raw map[string]any
	if err := json.Unmarshal([]byte(s), &raw); err != nil {
		return cfg
	}

	if v, ok := raw["enabled"].(bool); ok {
		cfg.Enabled = v
	}
	if v, ok := raw["apiKey"].(string); ok {
		cfg.APIKey = v
	}
	if v, ok := raw["commanderName"].(string); ok {
		cfg.CommanderName = v
	}
	if v, ok := raw["isBeingDeveloped"].(bool); ok {
		cfg.IsBeingDeveloped = v
	}
	if v, ok := raw["batchSize"].(float64); ok && v >= 1 {
		cfg.BatchSize = int(v)
	}
	if v, ok := raw["minIntervalSeconds"].(float64); ok && v >= 0 {
		cfg.MinIntervalSec = int(v)
	}
	if v, ok := raw["uploadHistorical"].(bool); ok {
		cfg.UploadHistorical = v
	}
	if v, ok := raw["dryRun"].(bool); ok {
		cfg.DryRun = v
	}
	return cfg
}
