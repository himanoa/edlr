package settings

import "testing"

// 既定でリプレイ分も送る。edlr が Journal の読み取り位置を永続化するように
// なったので、デーモン再起動をまたいだ重複送信は起きない。既定で捨てると、
// デーモンが止まっていた間のイベントが恒久的に失われるだけになる。
func TestUploadHistoricalDefaultsToTrue(t *testing.T) {
	if !Parse("{}").UploadHistorical {
		t.Fatal("uploadHistorical should default to true")
	}
}

func TestUploadHistoricalCanBeTurnedOff(t *testing.T) {
	if Parse(`{"uploadHistorical": false}`).UploadHistorical {
		t.Fatal("uploadHistorical=false must be honoured")
	}
}

func TestBrokenSettingsFallBackToDefaults(t *testing.T) {
	cfg := Parse("not json {{{")
	if cfg != Defaults() {
		t.Fatalf("unparsable settings must yield the defaults, got %+v", cfg)
	}
}

func TestOtherKeysStillParse(t *testing.T) {
	cfg := Parse(`{"enabled": false, "apiKey": "k", "batchSize": 3, "minIntervalSeconds": 0, "dryRun": true}`)
	if cfg.Enabled || cfg.APIKey != "k" || cfg.BatchSize != 3 || cfg.MinIntervalSec != 0 || !cfg.DryRun {
		t.Fatalf("unexpected settings: %+v", cfg)
	}
}
