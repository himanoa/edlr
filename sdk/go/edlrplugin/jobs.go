package edlrplugin

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
)

// Response は submit したジョブのデコード済み結果。Body は base64 を
// 復元したバイト列。
type Response struct {
	Status  uint16
	Headers [][2]string
	Body    []byte
}

type jobCallback func(*Response, error)

// wasm ゲストはシングルスレッドなので素の map でよい。
var pending = map[uint64]jobCallback{}

func registerPending(jobID uint64, cb jobCallback) { pending[jobID] = cb }

func takePending(jobID uint64) jobCallback {
	cb, ok := pending[jobID]
	if !ok {
		return nil
	}
	delete(pending, jobID)
	return cb
}

type jobResultJSON struct {
	OK *struct {
		Status     uint16      `json:"status"`
		Headers    [][2]string `json:"headers"`
		BodyBase64 string      `json:"body-base64"`
	} `json:"ok"`
	Err *struct {
		Kind    string `json:"kind"`
		Message string `json:"message"`
	} `json:"err"`
}

// parseJobResult は result-json(docs/plugins.md「非同期 HTTP」の形)を
// 値へ変換する純関数。
func parseJobResult(resultJSON string) (*Response, error) {
	var v jobResultJSON
	if err := json.Unmarshal([]byte(resultJSON), &v); err != nil {
		return nil, fmt.Errorf("result-json is not JSON: %w", err)
	}
	if v.Err != nil {
		return nil, fmt.Errorf("%s: %s", v.Err.Kind, v.Err.Message)
	}
	if v.OK == nil {
		return nil, fmt.Errorf("malformed result-json: neither ok nor err")
	}
	body, err := base64.StdEncoding.DecodeString(v.OK.BodyBase64)
	if err != nil {
		return nil, fmt.Errorf("invalid body-base64: %w", err)
	}
	return &Response{Status: v.OK.Status, Headers: v.OK.Headers, Body: body}, nil
}

// dispatchJobComplete は Register が Exports.OnJobComplete へ配線する実体。
// SDK 経由(SubmitHTTP)の job は pending から解決し、未知の id は
// Hooks.OnJobComplete へ委譲する。
func dispatchJobComplete(hooks Hooks) func(jobID uint64, resultJSON string) {
	return func(jobID uint64, resultJSON string) {
		if cb := takePending(jobID); cb != nil {
			cb(parseJobResult(resultJSON))
			return
		}
		if hooks.OnJobComplete != nil {
			hooks.OnJobComplete(jobID, resultJSON)
		}
	}
}
