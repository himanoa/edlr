package uploader

import (
	"errors"
	"testing"
	"time"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
)

// stubSender は送信を記録し、あらかじめ決めた結果を返す。
type stubSender struct {
	calls  [][]byte
	status int
	body   string
	err    error
}

func (s *stubSender) Send(body []byte) (int, []byte, error) {
	s.calls = append(s.calls, append([]byte(nil), body...))
	if s.err != nil {
		return 0, nil, s.err
	}
	return s.status, []byte(s.body), nil
}

func okSender() *stubSender {
	return &stubSender{status: 200, body: `{"header":{"eventStatus":200},"events":[]}`}
}

// clock は手で進める時計。
type clock struct{ t time.Time }

func (c *clock) now() time.Time          { return c.t }
func (c *clock) advance(d time.Duration) { c.t = c.t.Add(d) }

func newClock() *clock {
	return &clock{t: time.Date(2026, 7, 27, 0, 0, 0, 0, time.UTC)}
}

func testSettings() settings.Settings {
	cfg := settings.Defaults()
	cfg.APIKey = "key"
	cfg.CommanderName = "cmdr"
	return cfg
}

// jump は 1 件の INARA イベントになる Journal イベントを作る。
func jump(system string, replay bool) Event {
	return Event{
		Kind:      "journal",
		Name:      "FSDJump",
		Timestamp: "2026-07-27T00:00:00Z",
		Payload:   `{"StarSystem":"` + system + `"}`,
		Replay:    replay,
	}
}

func feed(u *Uploader, cfg settings.Settings, n int, replay bool) []Outcome {
	out := make([]Outcome, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, u.Handle(cfg, jump("Sol", replay)))
	}
	return out
}

// openLiveSession はゲーム版ゲートを開く(INARA は Live のデータしか
// 受け付けないため、バージョン学習前のイベントは全て捨てられる)。
// LoadGame 自体はイベントを生まない(Credits なし)ので、キューや
// フラッシュのカウントには影響しない。
func openLiveSession(u *Uploader) {
	u.Handle(testSettings(), Event{
		Kind:      "journal",
		Name:      "LoadGame",
		Timestamp: "2026-07-27T00:00:00Z",
		Payload:   `{"gameversion":"4.0.0.1904"}`,
		Replay:    true,
	})
}

func TestStatusEventsAreIgnored(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)

	out := u.Handle(testSettings(), Event{Kind: "status", Payload: `{}`})
	if out.Queued != 0 || len(sender.calls) != 0 {
		t.Errorf("status events must be ignored, got %+v", out)
	}
}

func TestDisabledPluginDoesNothing(t *testing.T) {
	sender := okSender()
	cfg := testSettings()
	cfg.Enabled = false
	u := New(newClock().now, sender)
	openLiveSession(u)

	if out := u.Handle(cfg, jump("Sol", false)); out.Queued != 0 {
		t.Errorf("a disabled plugin must not queue, got %+v", out)
	}
}

// replay モードは minIntervalSeconds を無視し、ReplayBatchSize ごとに送る。
func TestReplayFlushesEveryReplayBatchIgnoringTheInterval(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)

	feed(u, testSettings(), ReplayBatchSize-1, true)
	if len(sender.calls) != 0 {
		t.Fatalf("expected no upload before the batch is full, got %d", len(sender.calls))
	}

	// 時計は 1 秒も進めていない(minIntervalSeconds は 60)。
	out := u.Handle(testSettings(), jump("Sol", true))
	if len(sender.calls) != 1 {
		t.Fatalf("replay must ignore the interval, got %d uploads", len(sender.calls))
	}
	if out.Sent != ReplayBatchSize {
		t.Errorf("expected %d sent, got %d", ReplayBatchSize, out.Sent)
	}
}

// replay 中の Shutdown は過去のゲーム終了ログであって「もう来ない」を意味しない。
func TestReplayedShutdownDoesNotFlush(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", true))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`, Replay: true})

	if len(sender.calls) != 0 {
		t.Errorf("a replayed Shutdown must not flush, got %d uploads", len(sender.calls))
	}
	if out.Pending != 1 {
		t.Errorf("the event must stay queued, got %+v", out)
	}
}

// live に切り替わった瞬間、replay で溜まった端数を送り切る。
func TestTransitionToLiveFlushesTheBacklog(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	feed(u, cfg, 3, true)
	out := u.Handle(cfg, jump("Sol", false))

	if len(sender.calls) != 1 {
		t.Fatalf("expected one upload at the transition, got %d", len(sender.calls))
	}
	if out.Sent != 4 {
		t.Errorf("expected the backlog plus the live event, got %d", out.Sent)
	}
}

// replay 中に送れない状態(API キー未設定など)が続くと backoff が立ち、
// minIntervalSeconds が経つまで全速リトライしない。
func TestReplayBacksOffWhenItCannotSend(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.APIKey = ""

	outs := feed(u, cfg, ReplayBatchSize, true)
	last := outs[len(outs)-1]
	if last.Held == "" {
		t.Fatalf("expected the first flush attempt to be held, got %+v", last)
	}

	// 時計を進めずに追加の replay イベントを流す。backoff が立っているので
	// 全速リトライは起きず、Held は再び現れないはずである。
	for _, out := range feed(u, cfg, 5, true) {
		if out.Held != "" {
			t.Errorf("expected no retry before the interval elapses, got %+v", out)
		}
	}

	c.advance(time.Duration(cfg.MinIntervalSec) * time.Second)
	retry := u.Handle(cfg, jump("Sol", true))
	if retry.Held == "" {
		t.Errorf("expected the retry to be attempted once the interval elapsed, got %+v", retry)
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded without an api key, got %d", len(sender.calls))
	}
}

// バックログが無ければ、最初の live イベントだけでは送信しない
// (batchSize/minIntervalSeconds を満たすまで待つ)。
func TestFirstLiveEventWithoutBacklogDoesNotFlush(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	out := u.Handle(cfg, jump("Sol", false))
	if len(sender.calls) != 0 {
		t.Errorf("the first live event alone must not flush, got %d uploads / %+v", len(sender.calls), out)
	}
}

// uploadHistorical=false のスキップ件数は、遷移時に 1 回だけ報告する。
func TestSkippedReplayIsReportedOnceAtTheTransition(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.UploadHistorical = false

	for _, out := range feed(u, cfg, 5, true) {
		if out.Queued != 0 || out.Skipped != 0 {
			t.Fatalf("replayed events must be skipped silently, got %+v", out)
		}
	}

	out := u.Handle(cfg, jump("Sol", false))
	if out.Skipped != 5 {
		t.Errorf("expected 5 skipped reported at the transition, got %+v", out)
	}
	if next := u.Handle(cfg, jump("Sol", false)); next.Skipped != 0 {
		t.Errorf("the skipped total must be reported only once, got %+v", next)
	}
}

// live モードは batchSize と minIntervalSeconds の両方を満たすまで送らない。
func TestLiveRespectsBatchSizeAndInterval(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.BatchSize = 3

	feed(u, cfg, 3, false)
	if len(sender.calls) != 0 {
		t.Fatalf("expected no upload before the interval elapses, got %d", len(sender.calls))
	}

	c.advance(time.Duration(cfg.MinIntervalSec) * time.Second)
	out := u.Handle(cfg, jump("Sol", false))
	if len(sender.calls) != 1 {
		t.Fatalf("expected an upload once the interval elapsed, got %d", len(sender.calls))
	}
	if out.Sent != 4 {
		t.Errorf("expected 4 sent, got %d", out.Sent)
	}
}

func TestLiveShutdownFlushesImmediately(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(sender.calls) != 1 || out.Sent != 1 {
		t.Errorf("a live Shutdown must flush, got %d uploads / %+v", len(sender.calls), out)
	}
}

// 現行実装の実質バグの回帰テスト: 送れない状態でもキューは上限で止まる。
func TestQueueStaysBoundedWithoutAnApiKey(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.APIKey = ""

	var lastPending, totalDropped int
	for i := 0; i < MaxQueued*3; i++ {
		out := u.Handle(cfg, jump("Sol", false))
		lastPending = out.Pending
		totalDropped += out.Dropped
		c.advance(time.Minute)
	}

	if lastPending > MaxQueued {
		t.Errorf("the queue must stay bounded, got %d pending", lastPending)
	}
	if totalDropped == 0 {
		t.Error("dropped events must be reported")
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded without an api key, got %d", len(sender.calls))
	}
}

func TestHoldsWhenTheCommanderIsUnknown(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.CommanderName = ""

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if out.Held == "" {
		t.Error("expected a held reason")
	}
	if out.Pending != 1 {
		t.Errorf("held events must stay queued, got %+v", out)
	}
	if len(sender.calls) != 0 {
		t.Errorf("nothing may be uploaded, got %d", len(sender.calls))
	}
}

// コマンダー名は Journal から学習できる(設定が空でも送れるようになる)。
func TestCommanderNameIsLearnedFromTheJournal(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.CommanderName = ""

	u.Handle(cfg, Event{Kind: "journal", Name: "Commander", Payload: `{"Name":"Hutton","FID":"F1"}`})
	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if out.Held != "" || out.Sent != 1 {
		t.Fatalf("expected the learned name to allow the upload, got %+v", out)
	}
}

func TestDryRunReturnsTheBodyWithoutSending(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.DryRun = true

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(sender.calls) != 0 {
		t.Error("dry run must not send")
	}
	if len(out.DryRun) == 0 || out.Sent != 1 || out.Pending != 0 {
		t.Errorf("unexpected outcome: %+v", out)
	}
}

// 送信失敗はキューを残して次のイベントで再試行する。
func TestTransportFailureKeepsTheQueue(t *testing.T) {
	sender := &stubSender{err: errors.New("timeout")}
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})
	if out.Err == nil || out.Fatal {
		t.Errorf("expected a retryable error, got %+v", out)
	}
	if out.Pending != 1 {
		t.Errorf("the queue must be kept for a retry, got %+v", out)
	}

	sender.err = nil
	sender.status = 200
	sender.body = `{"header":{"eventStatus":200},"events":[]}`
	c.advance(time.Minute)
	retry := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})
	if retry.Sent != 1 || retry.Pending != 0 {
		t.Errorf("expected the retry to succeed, got %+v", retry)
	}
}

// バッチ全体の拒否は恒久的なのでキューを捨てる。
func TestBatchRejectionDropsTheQueue(t *testing.T) {
	sender := &stubSender{status: 200, body: `{"header":{"eventStatus":400,"eventStatusText":"bad key"},"events":[]}`}
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if !out.Fatal || out.Err == nil {
		t.Errorf("expected a fatal error, got %+v", out)
	}
	if out.Pending != 0 || out.Dropped != 1 {
		t.Errorf("a rejected batch must be dropped, got %+v", out)
	}
}

// 恒久失敗(バッチ拒否)の経路では backoff が解除されてはならない。解除される
// と、replay 中に API キー不正のような恒久エラーが起きても shouldFlush が
// intervalElapsed を見ずに次の ReplayBatchSize 件で即座にまた送信してしまい、
// minIntervalSeconds を無視して INARA を連打することになる。
func TestReplayBacksOffAfterABatchRejection(t *testing.T) {
	sender := &stubSender{status: 200, body: `{"header":{"eventStatus":400,"eventStatusText":"bad key"},"events":[]}`}
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()

	// ReplayBatchSize の 2.5 倍ほど流す。時計は一切進めない
	// (minIntervalSeconds は 60 秒)。
	feed(u, cfg, ReplayBatchSize*2+ReplayBatchSize/2, true)

	if len(sender.calls) != 1 {
		t.Fatalf("expected backoff to prevent further attempts before the interval elapses, got %d calls", len(sender.calls))
	}
}

// ヘッダの eventStatus 204 は INARA API v1 では「形式上は成功」('Soft'
// error)。Fatal を立てて送信済みのキューを捨てる(静かなデータ損失)のは
// 誤りで、送信は成功扱いのままキューを捨てる(=再送しない)のが正しい。
func TestHeaderSoftErrorIsTreatedAsSuccess(t *testing.T) {
	sender := &stubSender{status: 200, body: `{"header":{"eventStatus":204,"eventStatusText":"no results"},"events":[]}`}
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if out.Fatal {
		t.Errorf("a 204 header must not be fatal, got %+v", out)
	}
	if out.Sent != 1 {
		t.Errorf("a 204 header is a formal success, expected it counted as sent, got %+v", out)
	}
	if out.Pending != 0 {
		t.Errorf("the queue must be discarded after a formally successful send, got %+v", out)
	}
	if out.Warning == "" {
		t.Errorf("expected the 204 to be reported as a warning, got %+v", out)
	}
}

func TestIndividualRejectionsAreReported(t *testing.T) {
	sender := &stubSender{
		status: 200,
		body:   `{"header":{"eventStatus":200},"events":[{"eventStatus":400,"eventStatusText":"nope"}]}`,
	}
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})

	if len(out.Rejected) != 1 || out.Rejected[0].StatusText != "nope" {
		t.Errorf("unexpected rejections: %+v", out.Rejected)
	}
	if out.Pending != 0 {
		t.Errorf("a rejected event is not retried, got %+v", out)
	}
}

func TestBrokenPayloadIsReportedButDoesNotStopTheUploader(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	out := u.Handle(cfg, Event{Kind: "journal", Name: "FSDJump", Payload: "{oops"})
	if out.Err == nil {
		t.Fatal("expected an error for a broken payload")
	}

	next := u.Handle(cfg, jump("Sol", false))
	if next.Queued != 1 {
		t.Errorf("the uploader must keep working, got %+v", next)
	}
}

// batchSize が上限より大きくても、キューが詰まったまま止まらない。
func TestOversizedBatchSizeStillFlushes(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()
	cfg.BatchSize = MaxQueued * 10

	for i := 0; i < MaxQueued+1; i++ {
		u.Handle(cfg, jump("Sol", false))
		c.advance(time.Minute)
	}

	if len(sender.calls) == 0 {
		t.Error("an oversized batchSize must not wedge the queue")
	}
}

// Attempted は実際に HTTP 送信へ進んだときだけ立つ(開発モードの
// 「送信を試みた」ログの根拠になる)。成功・失敗を問わず、試行が
// あればバッチ件数が入る。
func TestAttemptedCountsRealSendAttempts(t *testing.T) {
	sender := &stubSender{err: errors.New("timeout")}
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.Handle(cfg, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`})
	if out.Attempted != 1 {
		t.Errorf("a failed send is still an attempt, got %+v", out)
	}
}

// スケジュール発火は minIntervalSeconds を経過していれば端数もフラッシュする。
func TestScheduleFlushesAPartialBatchAfterTheInterval(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	if len(sender.calls) != 0 {
		t.Fatalf("expected no upload before the schedule fires, got %d", len(sender.calls))
	}

	c.advance(time.Minute)
	out := u.HandleSchedule(cfg)
	if len(sender.calls) != 1 || out.Sent != 1 {
		t.Errorf("expected the schedule to flush the partial batch, got %d uploads / %+v", len(sender.calls), out)
	}
}

// スケジュール発火も minIntervalSeconds を尊重する。
func TestScheduleRespectsTheMinimumInterval(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.HandleSchedule(cfg)
	if out.Attempted != 0 || len(sender.calls) != 0 {
		t.Errorf("expected the schedule to respect the interval, got %d uploads / %+v", len(sender.calls), out)
	}
}

// キューが空のときスケジュール発火は何もしない。
func TestScheduleWithAnEmptyQueueDoesNothing(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()

	c.advance(time.Minute)
	out := u.HandleSchedule(cfg)
	if len(sender.calls) != 0 || out.Attempted != 0 {
		t.Errorf("expected nothing to happen with an empty queue, got %d uploads / %+v", len(sender.calls), out)
	}
}

// replay 中はスケジュール発火が割り込んで送ってはならない。ReplayBatchSize
// ごとにまとめて送る Handle 側のバッチングが、端数の先出しで壊れるため。
func TestScheduleDuringReplayDoesNotFlush(t *testing.T) {
	sender := okSender()
	c := newClock()
	u := New(c.now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", true))
	if u.sawLive {
		t.Fatalf("precondition: sawLive must still be false during replay")
	}

	c.advance(time.Minute)
	out := u.HandleSchedule(cfg)
	if out.Attempted != 0 || len(sender.calls) != 0 {
		t.Errorf("expected the schedule to skip during replay, got %d uploads / %+v", len(sender.calls), out)
	}
	if out.Pending != 1 {
		t.Errorf("expected the queue to stay intact during replay, got %+v", out)
	}
}

// on-stop は replay 中であっても無条件にフラッシュする(意図した仕様: 送れる
// ものは送ってから終了する)。ReplayBatchSize 未満の半端な件数を送ることに
// なっても、次のイベントはもう来ないので許容する。
func TestStopDuringReplayFlushesAnyway(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", true))
	out := u.HandleStop(cfg)
	if len(sender.calls) != 1 || out.Sent != 1 {
		t.Errorf("expected on-stop to flush even during replay, got %d uploads / %+v", len(sender.calls), out)
	}
}

// on-stop は間隔を無視して即座にフラッシュする(最後の機会)。
func TestStopFlushesImmediatelyIgnoringTheInterval(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)
	cfg := testSettings()

	u.Handle(cfg, jump("Sol", false))
	out := u.HandleStop(cfg)
	if len(sender.calls) != 1 || out.Sent != 1 {
		t.Errorf("expected on-stop to flush immediately, got %d uploads / %+v", len(sender.calls), out)
	}
}

func TestAttemptedStaysZeroWithoutASend(t *testing.T) {
	sender := okSender()
	u := New(newClock().now, sender)
	openLiveSession(u)

	// キューに積んだだけ(フラッシュ条件を満たさない)。
	cfg := testSettings()
	if out := u.Handle(cfg, jump("Sol", false)); out.Attempted != 0 {
		t.Errorf("queueing is not an attempt, got %+v", out)
	}

	// apiKey 未設定の Held。
	held := testSettings()
	held.APIKey = ""
	if out := u.Handle(held, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`}); out.Attempted != 0 {
		t.Errorf("holding is not an attempt, got %+v", out)
	}

	// dry run は送信しない。
	dry := testSettings()
	dry.DryRun = true
	if out := u.Handle(dry, Event{Kind: "journal", Name: "Shutdown", Payload: `{}`}); out.Attempted != 0 {
		t.Errorf("dry run is not an attempt, got %+v", out)
	}
	if len(sender.calls) != 0 {
		t.Fatalf("no real send may happen in this test, got %d", len(sender.calls))
	}
}
