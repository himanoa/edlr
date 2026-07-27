package inara

import (
	"errors"
	"strconv"
	"testing"
)

const okBody = `{"header":{"eventStatus":200,"eventStatusText":"OK"},"events":[{"eventStatus":200}]}`

func TestInterpretAcceptsSuccessfulBatch(t *testing.T) {
	res, err := Interpret(200, []byte(okBody))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(res.Rejected) != 0 {
		t.Errorf("expected no rejections, got %v", res.Rejected)
	}
}

// バッチ全体の拒否は恒久的(API キー不正など)。呼び出し側はキューを捨てる。
func TestInterpretReportsBatchRejectionAsBatchError(t *testing.T) {
	body := `{"header":{"eventStatus":400,"eventStatusText":"Invalid API key"},"events":[]}`
	_, err := Interpret(200, []byte(body))

	var batch *BatchError
	if !errors.As(err, &batch) {
		t.Fatalf("expected a *BatchError, got %v", err)
	}
	if batch.Status != 400 || batch.StatusText != "Invalid API key" {
		t.Errorf("unexpected batch error: %+v", batch)
	}
}

// 個別イベントの拒否はバッチ全体の失敗ではない。報告だけして再送はしない。
func TestInterpretReportsRejectedEvents(t *testing.T) {
	body := `{"header":{"eventStatus":200},"events":[{"eventStatus":200},{"eventStatus":400,"eventStatusText":"nope"}]}`
	res, err := Interpret(200, []byte(body))
	if err != nil {
		t.Fatalf("individual rejections must not fail the batch: %v", err)
	}
	if len(res.Rejected) != 1 {
		t.Fatalf("expected 1 rejection, got %v", res.Rejected)
	}
	if res.Rejected[0].Index != 1 || res.Rejected[0].Status != 400 || res.Rejected[0].StatusText != "nope" {
		t.Errorf("unexpected rejection: %+v", res.Rejected[0])
	}
}

// 非 2xx と壊れた JSON は一時的な失敗として扱う(BatchError ではないので再送される)。
func TestInterpretRejectsNon2xx(t *testing.T) {
	_, err := Interpret(503, []byte(okBody))
	if err == nil {
		t.Fatal("expected an error for HTTP 503")
	}
	var batch *BatchError
	if errors.As(err, &batch) {
		t.Error("an HTTP-level failure must be retryable, not a BatchError")
	}
}

func TestInterpretRejectsUnparsableBody(t *testing.T) {
	if _, err := Interpret(200, []byte("not json")); err == nil {
		t.Fatal("expected an error for an unparsable body")
	}
}

// ヘッダの eventStatus 202/204 は INARA API v1 では「形式上は成功」
// (Warning / 'Soft' error)。BatchError にせず、警告として報告する。
func TestInterpretTreatsHeader202And204AsSuccess(t *testing.T) {
	for _, status := range []int{202, 204} {
		body := `{"header":{"eventStatus":` + strconv.Itoa(status) + `,"eventStatusText":"partial"},"events":[]}`
		res, err := Interpret(200, []byte(body))
		if err != nil {
			t.Fatalf("eventStatus %d must not fail the batch: %v", status, err)
		}
		if res.Warning == "" {
			t.Errorf("eventStatus %d must produce a warning, got %+v", status, res)
		}
	}
}

// 400 以上は恒久的な失敗として BatchError を返す。
func TestInterpretTreatsHeader400AsBatchError(t *testing.T) {
	body := `{"header":{"eventStatus":400,"eventStatusText":"bad key"},"events":[]}`
	_, err := Interpret(200, []byte(body))

	var batch *BatchError
	if !errors.As(err, &batch) {
		t.Fatalf("expected a *BatchError, got %v", err)
	}
}

// 個別イベントの 204 は拒否ではなく警告として扱う(再送しない点は同じ)。
func TestInterpretTreatsEvent204AsWarningNotRejection(t *testing.T) {
	body := `{"header":{"eventStatus":200},"events":[{"eventStatus":204,"eventStatusText":"no results"}]}`
	res, err := Interpret(200, []byte(body))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(res.Rejected) != 0 {
		t.Errorf("a 204 must not be a rejection, got %+v", res.Rejected)
	}
	if len(res.Warned) != 1 || res.Warned[0].Status != 204 {
		t.Errorf("expected the 204 to be reported as a warning, got %+v", res.Warned)
	}
}
