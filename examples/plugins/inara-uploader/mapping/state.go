package mapping

// State はイベントをまたいで覚えておく情報。プラグインのプロセス内にしか無く、
// 永続化はされない(デーモンを再起動すると Journal の replay で埋め直される)。
type State struct {
	CommanderName string
	FrontierID    string
	// LastSystem は直近に確認した星系。Died は星系名を含まないため、
	// 移動系イベントから覚えておいたものを添える。
	LastSystem string

	// ranks / progress は INARA の rankName をキーにした段位と進捗。
	// Journal では別イベントで来るため、揃うまで送れない(ranks.go 参照)。
	ranks    map[string]int
	progress map[string]float64
}

func NewState() *State {
	return &State{
		ranks:    map[string]int{},
		progress: map[string]float64{},
	}
}

// learnIdentity は空でない値だけを取り込む。Journal のイベントによっては
// 片方しか入っていないため、上書きで消さないようにする。
func (s *State) learnIdentity(name, frontierID string) {
	if name != "" {
		s.CommanderName = name
	}
	if frontierID != "" {
		s.FrontierID = frontierID
	}
}
