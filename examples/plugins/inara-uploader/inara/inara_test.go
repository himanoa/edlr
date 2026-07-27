package inara

import (
	"encoding/json"
	"testing"
)

func TestEncodeUsesInaraFieldNames(t *testing.T) {
	body, err := Encode(
		Header{AppName: "app", APIKey: "k", CommanderName: "cmdr"},
		[]Event{{Name: "setCommanderCredits", Timestamp: "2026-07-27T00:00:00Z", Data: map[string]int{"commanderCredits": 1}}},
	)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}

	var got map[string]any
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatalf("Encode produced invalid JSON: %v", err)
	}
	header := got["header"].(map[string]any)
	if header["APIkey"] != "k" {
		t.Errorf("api key must be marshalled as APIkey, got %v", header)
	}
	events := got["events"].([]any)
	first := events[0].(map[string]any)
	if first["eventName"] != "setCommanderCredits" || first["eventTimestamp"] != "2026-07-27T00:00:00Z" {
		t.Errorf("unexpected event shape: %v", first)
	}
}

// FrontierID は Journal から学習できていないことがあるので、空なら落とす。
func TestEncodeOmitsEmptyFrontierID(t *testing.T) {
	body, err := Encode(Header{CommanderName: "cmdr"}, nil)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}
	var got struct {
		Header map[string]any `json:"header"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatal(err)
	}
	if _, ok := got.Header["commanderFrontierID"]; ok {
		t.Errorf("empty frontier id must be omitted, got %v", got.Header)
	}
}
