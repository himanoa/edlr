package mapping

import "testing"

// デーモンをセッション途中で再起動すると、Journal の読み取り位置は永続化
// されている一方で LoadGame は再配信されない。永続化した State を読み戻す
// ことで、Live ゲートとコマンダー名が再起動をまたいで生き残ることを確認する。
func TestStateRoundTripPreservesLiveGateAndIdentity(t *testing.T) {
	s := NewState()
	s.learnGameVersion("4.0.0.1904")
	s.learnIdentity("Commander Foo", "F123")
	s.LastSystem = "Sol"
	s.ranks["combat"] = 5
	s.progress["combat"] = 42.5

	raw, err := s.Marshal()
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	got := UnmarshalState(raw)

	if !got.liveAllowed() {
		t.Error("復元後の State が Live ゲートを通しません")
	}
	if got.CommanderName != "Commander Foo" || got.FrontierID != "F123" {
		t.Errorf("identity が失われました: %+v", got)
	}
	if got.LastSystem != "Sol" {
		t.Errorf("LastSystem が失われました: %q", got.LastSystem)
	}
	if got.ranks["combat"] != 5 || got.progress["combat"] != 42.5 {
		t.Errorf("ranks/progress が失われました: %v %v", got.ranks, got.progress)
	}
}

func TestUnmarshalStateToleratesGarbage(t *testing.T) {
	for _, data := range [][]byte{nil, []byte(""), []byte("{broken")} {
		s := UnmarshalState(data)
		if s == nil || s.ranks == nil || s.progress == nil {
			t.Fatalf("壊れた入力 %q から使える State が返りません", data)
		}
		if s.liveAllowed() {
			t.Errorf("空の State が Live ゲートを通してはいけません")
		}
	}
}
