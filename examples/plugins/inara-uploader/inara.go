package main

import (
	"encoding/json"
	"fmt"

	"go.bytecodealliance.org/cm"

	driverhttp "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/driver-http"
	hostlog "github.com/himanoa/edlr/examples/plugins/inara-uploader/gen/edlr/plugin/host-log"
)

// inaraEndpoint は INARA API v1 のエンドポイント。manifest の
// `[[capabilities]]` で `https://inara.cz` を要求し、ユーザーが承認した
// 場合にだけ `driver-http.send` が通る。
const inaraEndpoint = "https://inara.cz/inapi/v1/"

type inaraHeader struct {
	AppName          string `json:"appName"`
	AppVersion       string `json:"appVersion"`
	IsBeingDeveloped bool   `json:"isBeingDeveloped"`
	APIKey           string `json:"APIkey"`
	CommanderName    string `json:"commanderName"`
	FrontierID       string `json:"commanderFrontierID,omitempty"`
}

type inaraEvent struct {
	EventName      string `json:"eventName"`
	EventTimestamp string `json:"eventTimestamp"`
	EventData      any    `json:"eventData"`
}

type inaraRequest struct {
	Header inaraHeader  `json:"header"`
	Events []inaraEvent `json:"events"`
}

// inaraResponse は INARA の応答のうち、このプラグインが解釈する部分。
type inaraResponse struct {
	Header struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"header"`
	Events []struct {
		EventStatus     int    `json:"eventStatus"`
		EventStatusText string `json:"eventStatusText"`
	} `json:"events"`
}

// postToInara は組み立て済みの JSON を INARA へ POST する。
//
// `driver-http.send` はホスト側で 1.5 秒のタイムアウトが掛かっており、
// リダイレクトも追わない。タイムアウトやネットワークエラーは
// `transport` として返るので、呼び出し側はキューを保持して再試行する。
func postToInara(body []byte) (*inaraResponse, error) {
	req := driverhttp.Request{
		Method: "POST",
		URL:    inaraEndpoint,
		Headers: cm.ToList([][2]string{
			{"content-type", "application/json"},
			{"accept", "application/json"},
		}),
		Body: cm.Some(cm.ToList(body)),
	}

	result := driverhttp.Send(req)
	if err := result.Err(); err != nil {
		return nil, fmt.Errorf("%s: %s", err.String(), driverErrorMessage(err))
	}

	resp := result.OK()
	if resp.Status < 200 || resp.Status >= 300 {
		return nil, fmt.Errorf("inara returned HTTP %d", resp.Status)
	}

	var parsed inaraResponse
	if err := json.Unmarshal(resp.Body.Slice(), &parsed); err != nil {
		return nil, fmt.Errorf("unparsable inara response: %w", err)
	}
	return &parsed, nil
}

// driverErrorMessage は variant の中身(理由文字列)を取り出す。
func driverErrorMessage(err *driverhttp.DriverError) string {
	if m := err.PermissionDenied(); m != nil {
		return *m
	}
	if m := err.InvalidRequest(); m != nil {
		return *m
	}
	if m := err.Transport(); m != nil {
		return *m
	}
	return "unknown driver error"
}

// logResult は INARA の応答をログへ落とす。
//
// INARA はバッチ全体のステータス(header)と、イベントごとのステータスを
// 別々に返す。全体が成功でも個々のイベントが拒否されることがあるため、
// 両方を見る。
func logResult(resp *inaraResponse, sent int) {
	if resp.Header.EventStatus != 200 {
		logf(hostlog.LevelError, "inara rejected the batch: %d %s",
			resp.Header.EventStatus, resp.Header.EventStatusText)
		return
	}

	failed := 0
	for _, ev := range resp.Events {
		if ev.EventStatus != 200 {
			failed++
			logf(hostlog.LevelWarn, "inara rejected an event: %d %s",
				ev.EventStatus, ev.EventStatusText)
		}
	}

	if failed == 0 {
		logf(hostlog.LevelInfo, "uploaded %d event(s) to inara", sent)
		return
	}
	logf(hostlog.LevelWarn, "uploaded %d event(s) to inara, %d rejected", sent, failed)
}
