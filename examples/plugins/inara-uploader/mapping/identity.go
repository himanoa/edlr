package mapping

import "github.com/himanoa/edlr/examples/plugins/inara-uploader/inara"

// commander はコマンダー名の学習だけに使う(送信するものは無い)。
type commander struct {
	Name string `json:"Name"`
	FID  string `json:"FID"`
}

func (c commander) convert(st *State) []inara.Event {
	st.learnIdentity(c.Name, c.FID)
	return nil
}

type loadGame struct {
	Commander   string `json:"Commander"`
	FID         string `json:"FID"`
	Credits     *int64 `json:"Credits"`
	Loan        *int64 `json:"Loan"`
	GameVersion string `json:"gameversion"`
}

// credits の Loan はポインタ。0 が有効値(借金なし)なので omitempty では
// 「借金を返した」と「Journal に入っていない」を区別できない。
type credits struct {
	Credits int64  `json:"commanderCredits"`
	Loan    *int64 `json:"commanderLoan,omitempty"`
}

func (g loadGame) convert(st *State) []inara.Event {
	// 学習(identity / バージョン)は Live ゲートより先に行う -- Legacy
	// セッションでも「誰か」「どの版か」は覚えておき、送信だけを止める。
	st.learnGameVersion(g.GameVersion)
	st.learnIdentity(g.Commander, g.FID)
	if g.Credits == nil {
		return nil
	}
	return []inara.Event{inara.New("setCommanderCredits", credits{
		Credits: *g.Credits,
		Loan:    g.Loan,
	})}
}
