package edlrplugin

import (
	"reflect"
	"testing"
)

func TestParseJobResultOK(t *testing.T) {
	json := `{"ok":{"status":200,"headers":[["x-a","b"]],"body-base64":"aGVsbG8="}}`
	resp, err := parseJobResult(json)
	if err != nil {
		t.Fatalf("expected ok, got %v", err)
	}
	if resp.Status != 200 || !reflect.DeepEqual(resp.Body, []byte("hello")) {
		t.Fatalf("bad decode: %+v", resp)
	}
	if len(resp.Headers) != 1 || resp.Headers[0] != [2]string{"x-a", "b"} {
		t.Fatalf("bad headers: %+v", resp.Headers)
	}
}

func TestParseJobResultErrKinds(t *testing.T) {
	for _, tc := range []struct{ json, want string }{
		{`{"err":{"kind":"transport","message":"boom"}}`, "transport: boom"},
		{`{"err":{"kind":"invalid-request","message":"bad"}}`, "invalid-request: bad"},
	} {
		if _, err := parseJobResult(tc.json); err == nil || err.Error() != tc.want {
			t.Fatalf("json %s: got %v, want %s", tc.json, err, tc.want)
		}
	}
}

func TestParseJobResultMalformed(t *testing.T) {
	for _, json := range []string{"not json", "{}", `{"ok":{"status":200,"headers":[],"body-base64":"%%%"}}`} {
		if _, err := parseJobResult(json); err == nil {
			t.Fatalf("json %s: expected an error", json)
		}
	}
}

func TestPendingResolvesOnceAndUnknownDelegates(t *testing.T) {
	resolved := 0
	registerPending(7, func(*Response, error) { resolved++ })
	if cb := takePending(7); cb == nil {
		t.Fatal("registered id must resolve")
	} else {
		cb(nil, nil)
	}
	if takePending(7) != nil {
		t.Fatal("second take must be nil (resolve exactly once)")
	}
	if takePending(999) != nil {
		t.Fatal("unknown id must be nil")
	}
	if resolved != 1 {
		t.Fatalf("callback ran %d times", resolved)
	}
}
