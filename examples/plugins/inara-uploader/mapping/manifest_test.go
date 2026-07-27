package mapping

import (
	"os"
	"strings"
	"testing"
)

// manifestEvents は manifest.toml の `events = [...]` から名前を拾う。
//
// TOML パーサを go.mod に足すほどではない(読むのは自分たちが書いた 1
// ファイルの 1 配列だけ)。想定するのは複数行の配列で、要素は 1 行に 1 つ。
func manifestEvents(t *testing.T) []string {
	t.Helper()

	raw, err := os.ReadFile("../manifest.toml")
	if err != nil {
		t.Fatalf("cannot read manifest.toml: %v", err)
	}

	var names []string
	inArray := false
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		switch {
		case !inArray && strings.HasPrefix(line, "events") && strings.HasSuffix(line, "["):
			inArray = true
		case inArray && strings.HasPrefix(line, "]"):
			return names
		case inArray:
			if name, ok := quoted(line); ok {
				names = append(names, name)
			}
		}
	}
	t.Fatal("manifest.toml has no multi-line `events = [` array")
	return nil
}

// quoted は `"Docked",` のような行から中身を取り出す。コメント行と空行は false。
func quoted(line string) (string, bool) {
	start := strings.Index(line, `"`)
	if start < 0 {
		return "", false
	}
	end := strings.Index(line[start+1:], `"`)
	if end < 0 {
		return "", false
	}
	return line[start+1 : start+1+end], true
}

// manifest の events とレジストリのキーは一致していること。ズレると
// 「manifest に書いたのに変換されない」「変換を書いたのにイベントが
// 届かない」のどちらかが黙って起きる。
func TestManifestEventsMatchTheRegistry(t *testing.T) {
	manifest := map[string]bool{}
	for _, name := range manifestEvents(t) {
		manifest[name] = true
	}

	registry := map[string]bool{}
	for _, name := range Names() {
		registry[name] = true
	}

	for name := range manifest {
		if !registry[name] {
			t.Errorf("manifest.toml subscribes to %q but mapping has no handler for it", name)
		}
	}
	for name := range registry {
		if !manifest[name] {
			t.Errorf("mapping handles %q but manifest.toml does not subscribe to it", name)
		}
	}
}

func TestManifestEventsAreParsed(t *testing.T) {
	if got := manifestEvents(t); len(got) == 0 {
		t.Fatal("no events parsed from manifest.toml")
	}
}
