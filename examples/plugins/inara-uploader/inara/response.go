package inara

import (
	"encoding/json"
	"fmt"
)

// response は INARA の応答のうち、このプラグインが解釈する部分。
type response struct {
	Header struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"header"`
	Events []struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"events"`
}

// Rejection は INARA が個別に拒否したイベント。Index はバッチ内の位置。
type Rejection struct {
	Index      int
	Status     int
	StatusText string
}

// Result は応答の解釈結果。
type Result struct {
	Rejected []Rejection
}

// BatchError はバッチ全体が拒否されたことを表す。API キー不正などの恒久的な
// 失敗なので、同じ内容を送り直しても通らない。呼び出し側はキューを捨てる。
type BatchError struct {
	Status     int
	StatusText string
}

func (e *BatchError) Error() string {
	return fmt.Sprintf("inara rejected the batch: %d %s", e.Status, e.StatusText)
}

// Interpret は HTTP ステータスと応答本文を解釈する。
//
// *BatchError 以外のエラーは一時的な失敗として扱ってよい(呼び出し側は
// キューを保持して再試行する)。
func Interpret(status int, body []byte) (Result, error) {
	if status < 200 || status >= 300 {
		return Result{}, fmt.Errorf("inara returned HTTP %d", status)
	}

	var parsed response
	if err := json.Unmarshal(body, &parsed); err != nil {
		return Result{}, fmt.Errorf("unparsable inara response: %w", err)
	}

	if parsed.Header.EventStatus != 200 {
		return Result{}, &BatchError{
			Status:     parsed.Header.EventStatus,
			StatusText: parsed.Header.EventStatusText,
		}
	}

	var res Result
	for i, ev := range parsed.Events {
		if ev.EventStatus != 200 {
			res.Rejected = append(res.Rejected, Rejection{
				Index:      i,
				Status:     ev.EventStatus,
				StatusText: ev.EventStatusText,
			})
		}
	}
	return res, nil
}
