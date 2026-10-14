package exec_process

import (
	"os"
	"reflect"
	"sync/atomic"
	"time"

	"github.com/charmbracelet/x/term"
)

func observeTTYChanges(stdin *os.File) (stop func() bool, ok bool) {
	before, err := term.GetState(stdin.Fd())
	if err != nil {
		return nil, false
	}

	var changed atomic.Bool
	done := make(chan struct{})

	go func() {
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()

		for {
			select {
			case <-done:
				return
			case <-ticker.C:
				current, err := term.GetState(stdin.Fd())
				if err != nil {
					continue
				}
				if !reflect.DeepEqual(current, before) {
					changed.Store(true)
					return
				}
			}
		}
	}()

	return func() bool {
		close(done)
		return changed.Load()
	}, true
}
