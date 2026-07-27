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
	// Warning はヘッダの eventStatus が 200 以外の 2xx(202/204 など)だった
	// ことを表す。空なら警告なし。バッチは形式上成功しているのでキューは
	// 捨ててよいが、内容は呼び出し側がログに残せるようにしておく。
	Warning string
	// Rejected は 400 以上で個別に拒否されたイベント。
	Rejected []Rejection
	// Warned は 200 以外の 2xx だった個別イベント。拒否ではないので
	// 再送はしないが、警告としてログに出せるようにしておく。
	Warned []Rejection
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
//
// INARA API v1 の eventStatus は 200(OK)/202(Warning)/204('Soft' error) が
// いずれも「形式上は成功」で、400 以上だけが本当の失敗(認証エラーなどで
// バッチ全体がキャンセルされた)を表す。202/204 を失敗として扱うと、送信に
// 成功しているのに Fatal を立ててキューを捨てることになる(静かなデータ
// 損失)ので、判定は 400 以上かどうかで行う。
func Interpret(status int, body []byte) (Result, error) {
	if status < 200 || status >= 300 {
		return Result{}, fmt.Errorf("inara returned HTTP %d", status)
	}

	var parsed response
	if err := json.Unmarshal(body, &parsed); err != nil {
		return Result{}, fmt.Errorf("unparsable inara response: %w", err)
	}

	if parsed.Header.EventStatus >= 400 {
		return Result{}, &BatchError{
			Status:     parsed.Header.EventStatus,
			StatusText: parsed.Header.EventStatusText,
		}
	}

	var res Result
	if parsed.Header.EventStatus != 200 {
		res.Warning = fmt.Sprintf("%d %s", parsed.Header.EventStatus, parsed.Header.EventStatusText)
	}
	for i, ev := range parsed.Events {
		switch {
		case ev.EventStatus >= 400:
			res.Rejected = append(res.Rejected, Rejection{
				Index:      i,
				Status:     ev.EventStatus,
				StatusText: ev.EventStatusText,
			})
		case ev.EventStatus != 200:
			res.Warned = append(res.Warned, Rejection{
				Index:      i,
				Status:     ev.EventStatus,
				StatusText: ev.EventStatusText,
			})
		}
	}
	return res, nil
}
