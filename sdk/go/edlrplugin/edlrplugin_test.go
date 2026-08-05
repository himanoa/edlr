package edlrplugin

import (
	"testing"

	driverhttp "github.com/himanoa/edlr/sdk/go/gen/edlr/plugin/driver-http"
)

func TestDriverErrorMessageIncludesReason(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  driverhttp.DriverError
		want string
	}{
		{"permission-denied", driverhttp.DriverErrorPermissionDenied("host inara.cz not authorized"), "permission-denied: host inara.cz not authorized"},
		{"invalid-request", driverhttp.DriverErrorInvalidRequest("in-flight limit exceeded"), "invalid-request: in-flight limit exceeded"},
		{"transport", driverhttp.DriverErrorTransport("connection reset"), "transport: connection reset"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := driverErrorMessage(&tc.err); got != tc.want {
				t.Fatalf("got %q, want %q", got, tc.want)
			}
		})
	}
}
