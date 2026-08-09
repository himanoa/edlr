package uploader

import (
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"

	"github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/mapping"
	"github.com/himanoa/edlr/examples/plugins/inara-uploader/settings"
)

const (
	// ResyncChunkBytes は driver-fs の read-range 1 回で読む上限。
	// 1 バッチぶんの読み・変換・送信投入を CALL_DEADLINE(2 秒)内に
	// 収めるための保守的な値。
	ResyncChunkBytes = 128 * 1024
	// ResyncBatchSize は 1 リクエストに載せる INARA イベントの上限。
	// 通常運用の ReplayBatchSize と同じ。
	ResyncBatchSize = 100
)

// FS は再送エンジンが Journal を読み直すための最小の口。実体は main の
// driver-fs 呼び出し(manifest の [[filesystem]] name="journal")。
type FS interface {
	// List はルート直下のエントリ(相対パスとサイズ)を返す。
	List() ([]FileInfo, error)
	ReadRange(path string, offset uint64, n uint32) ([]byte, error)
}

type FileInfo struct {
	Path string
	Size uint64
}

// AsyncSender は組み立て済み JSON の非同期送信。実体は main の
// SubmitHTTP で、完了は Resync.OnSendResult へ返す。
type AsyncSender interface {
	Submit(body []byte) error
}

// ResyncOutcome は Start / OnSendResult 1 回で起きたこと。ログ整形は main。
type ResyncOutcome struct {
	// Refused は開始しなかった理由(空なら開始または継続)
	Refused string
	// Submitted は今回投げたイベント数(0 なら送信は発生していない)
	Submitted int
	// Sent は今回の完了応答で確定した送信数
	Sent int
	// OffsetBytes / SizeBytes は進捗表示用
	OffsetBytes uint64
	SizeBytes   uint64
	Rejected    []inara.Rejection
	Warned      []inara.Rejection
	Warning     string
	// Done は再送が終わった(完走・中断とも)。Err が nil なら完走
	Done bool
	// Total は Done 時の累計送信数
	Total int
	Err   error
}

// Resync は「現行セッションの Journal を読み直して INARA へ送り直す」
// 手動同期のステートマシン。1 本だけ走る(実行中の Start は無視)。
//
// 進行状態はメモリのみ: デーモンを再起動したらボタンを押し直す。
// 送信は 1 バッチずつの数珠つなぎ(Submit → OnSendResult → 次のバッチ)で、
// minIntervalSeconds は適用しない。変換は専用の mapping.State で行うので、
// ファイル先頭の LoadGame から学習し直し、Live と確認できないセッションは
// mapping が自然に捨てる。
type Resync struct {
	fs   FS
	send AsyncSender

	running bool
	file    string
	// size は開始時点のファイルサイズ。開始後の追記は通常経路が送るので
	// ここまでで打ち切る。
	size    uint64
	offset  uint64
	partial []byte
	state   *mapping.State
	batch   []inara.Event
	// inFlight は直前の Submit に載せた件数(batch の先頭からこの件数)。
	inFlight int
	eof      bool
	total    int
}

func NewResync(fs FS, send AsyncSender) *Resync {
	return &Resync{fs: fs, send: send}
}

// Start は再送を開始し、最初のバッチを投げる。
func (r *Resync) Start(cfg settings.Settings) ResyncOutcome {
	if reason := refuseReason(r.running, cfg); reason != "" {
		return ResyncOutcome{Refused: reason}
	}
	file, size, err := latestJournal(r.fs)
	if err != nil {
		return ResyncOutcome{Refused: fmt.Sprintf("journal を読めません: %v", err)}
	}
	if file == "" {
		return ResyncOutcome{Refused: "journal ディレクトリに Journal ファイルがありません"}
	}
	r.running = true
	r.file = file
	r.size = size
	r.offset = 0
	r.partial = nil
	r.state = mapping.NewState()
	r.batch = nil
	r.eof = size == 0
	r.total = 0
	return r.advance(cfg)
}

// OnSendResult は非同期送信の完了。結果を解釈し、続きを送るか終える。
func (r *Resync) OnSendResult(status int, body []byte, sendErr error, cfg settings.Settings) ResyncOutcome {
	if !r.running {
		// 完了前に何らかの理由で中断済み。黙って捨てるしかない。
		return ResyncOutcome{}
	}
	if sendErr != nil {
		// 手動操作なので自動リトライはしない。もう一度ボタンを押してもらう。
		return r.finish(ResyncOutcome{Err: fmt.Errorf("再送を中断しました: %w", sendErr)})
	}
	result, err := inara.Interpret(status, body)
	if err != nil {
		var rejected *inara.BatchError
		if errors.As(err, &rejected) {
			return r.finish(ResyncOutcome{Err: fmt.Errorf("INARA がバッチを拒否したため再送を中断しました: %w", err)})
		}
		return r.finish(ResyncOutcome{Err: fmt.Errorf("再送を中断しました: %w", err)})
	}
	sent := r.inFlight
	r.total += sent
	r.batch = r.batch[r.inFlight:]
	r.inFlight = 0
	out := r.advance(cfg)
	out.Sent = sent
	out.Rejected = result.Rejected
	out.Warned = result.Warned
	out.Warning = result.Warning
	return out
}

// advance は次のバッチを組み立てて投げる。送るものが尽きたら完了。
func (r *Resync) advance(cfg settings.Settings) ResyncOutcome {
	if err := r.fillBatch(); err != nil {
		return r.finish(ResyncOutcome{Err: fmt.Errorf("journal の読み取りに失敗し再送を中断しました: %w", err)})
	}
	if len(r.batch) == 0 {
		return r.finish(ResyncOutcome{})
	}
	commander := cfg.CommanderName
	if commander == "" {
		commander = r.state.CommanderName
	}
	if commander == "" {
		// fillBatch がイベントを返した時点で LoadGame は学習済みのはずだが、
		// 設定にも Journal にも名前が無いケースは送りようがない。
		return r.finish(ResyncOutcome{Err: errors.New("コマンダー名が分からないため再送を中断しました")})
	}
	sending := r.batch[:min(len(r.batch), ResyncBatchSize)]
	body, err := inara.Encode(inara.Header{
		AppName:          appName,
		AppVersion:       appVersion,
		IsBeingDeveloped: cfg.IsBeingDeveloped,
		APIKey:           cfg.APIKey,
		CommanderName:    commander,
		FrontierID:       r.state.FrontierID,
	}, sending)
	if err != nil {
		return r.finish(ResyncOutcome{Err: fmt.Errorf("バッチを組み立てられず再送を中断しました: %w", err)})
	}
	if err := r.send.Submit(body); err != nil {
		return r.finish(ResyncOutcome{Err: fmt.Errorf("送信を受け付けてもらえず再送を中断しました: %w", err)})
	}
	r.inFlight = len(sending)
	return ResyncOutcome{
		Submitted:   len(sending),
		OffsetBytes: r.offset,
		SizeBytes:   r.size,
	}
}

// fillBatch は batch が ResyncBatchSize に達するか EOF まで、チャンク読みと
// 変換を進める。
func (r *Resync) fillBatch() error {
	for len(r.batch) < ResyncBatchSize && !r.eof {
		want := min(uint64(ResyncChunkBytes), r.size-r.offset)
		buf, err := r.fs.ReadRange(r.file, r.offset, uint32(want))
		if err != nil {
			return err
		}
		if len(buf) == 0 {
			// 開始時点のサイズより先に縮んだ(ローテーション等)。打ち切る。
			r.eof = true
			break
		}
		r.offset += uint64(len(buf))
		r.eof = r.offset >= r.size
		r.convertLines(append(r.partial, buf...))
	}
	return nil
}

// convertLines は完全な行を変換して batch へ足し、末尾の不完全な行を
// partial として持ち越す。
func (r *Resync) convertLines(buf []byte) {
	idx := strings.LastIndexByte(string(buf), '\n')
	if idx < 0 {
		r.partial = buf
		return
	}
	lines := strings.Split(string(buf[:idx]), "\n")
	r.partial = append([]byte(nil), buf[idx+1:]...)
	for _, line := range lines {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var head struct {
			Timestamp string `json:"timestamp"`
			Event     string `json:"event"`
		}
		if err := json.Unmarshal([]byte(line), &head); err != nil || head.Event == "" {
			// 壊れた行は通常経路(parser)と同様に飛ばす
			continue
		}
		res, err := mapping.Convert(head.Event, head.Timestamp, json.RawMessage(line), r.state)
		if err != nil {
			// 変換できない 1 件のために全体を止めない
			continue
		}
		r.batch = append(r.batch, res.Events...)
	}
}

func (r *Resync) finish(out ResyncOutcome) ResyncOutcome {
	r.running = false
	r.batch = nil
	r.partial = nil
	r.state = nil
	out.Done = true
	out.Total = r.total
	return out
}

// refuseReason は Start を受け付けない理由(空なら受け付ける)。純関数。
func refuseReason(running bool, cfg settings.Settings) string {
	switch {
	case running:
		return "再送は既に実行中です"
	case !cfg.Enabled:
		return "プラグインが無効です(enabled)"
	case cfg.APIKey == "":
		return "apiKey が未設定です"
	case cfg.DryRun:
		return "dryRun が有効のため再送しません"
	}
	return ""
}

// latestJournal は一覧から最新の Journal ファイルを選ぶ。ファイル名は
// ISO 風タイムスタンプを含むため辞書順の最大が最新。
func latestJournal(fs FS) (string, uint64, error) {
	entries, err := fs.List()
	if err != nil {
		return "", 0, err
	}
	journals := make([]FileInfo, 0, len(entries))
	for _, e := range entries {
		if isJournalFile(e.Path) {
			journals = append(journals, e)
		}
	}
	if len(journals) == 0 {
		return "", 0, nil
	}
	sort.Slice(journals, func(i, j int) bool { return journals[i].Path < journals[j].Path })
	last := journals[len(journals)-1]
	return last.Path, last.Size, nil
}

func isJournalFile(path string) bool {
	return strings.HasPrefix(path, "Journal.") &&
		strings.HasSuffix(path, ".log") &&
		!strings.Contains(path, "/")
}
