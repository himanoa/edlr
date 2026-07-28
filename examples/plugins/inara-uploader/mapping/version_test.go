package mapping

import "testing"

func TestIsLiveVersion(t *testing.T) {
	cases := []struct {
		version string
		want    bool
	}{
		{"4.0.0.1904", true},
		{"4.1.2", true},
		{"5.0", true},
		{"3.8.0.404", false},
		{"3.8", false},
		{"4.0.0.100 beta", false},
		{"Beta 4.0", false},
		{"", false},
		{"garbage", false},
	}
	for _, c := range cases {
		if got := isLiveVersion(c.version); got != c.want {
			t.Errorf("isLiveVersion(%q) = %v, want %v", c.version, got, c.want)
		}
	}
}
