// Package privateclient is the only backend process boundary in fastjjui.
// Arguments travel over descriptor 3; optional UI prompts use descriptor 4.
// stdin/stdout/stderr retain their ordinary jj meaning.
package privateclient

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"

	"golang.org/x/sys/unix"
)

const maxRequest = 1024 * 1024
const maxPrompt = 128 * 1024

type PromptHandler func(prompt string, secret bool, done <-chan struct{}) []byte

var promptState struct {
	sync.RWMutex
	handler PromptHandler
}

func SetPromptHandler(handler PromptHandler) {
	promptState.Lock()
	defer promptState.Unlock()
	promptState.handler = handler
}

func executable() (string, error) {
	if ui, err := os.Executable(); err == nil {
		if resolved, err := filepath.EvalSymlinks(ui); err == nil {
			ui = resolved
		}
		sibling := filepath.Join(filepath.Dir(ui), "fastjj")
		if info, err := os.Stat(sibling); err == nil && !info.IsDir() && info.Mode()&0111 != 0 {
			return sibling, nil
		}
	}
	if backend, err := exec.LookPath("fastjj"); err == nil {
		return backend, nil
	}
	return "", errors.New("fastjj 0.2 or newer is required; install it beside fastjjui or on PATH")
}

// Probe refuses to fall back to an ordinary jj client or an incompatible fastjj.
func Probe() error {
	backend, err := executable()
	if err != nil {
		return err
	}
	cmd := exec.Command(backend, "--interface-version")
	cmd.Args[0] = "fastjj"
	output, err := cmd.Output()
	if err != nil || strings.TrimSpace(string(output)) != "fastjj-private-v1 jj=0.37.0" {
		return errors.New("fastjjui requires fastjj with the private v1 interface and jj 0.37.0")
	}
	return nil
}

type request struct {
	Args   []string `json:"args"`
	Cwd    string   `json:"cwd,omitempty"`
	Prompt bool     `json:"prompt,omitempty"`
}

// Command prepares a child with constant argv. Close must always be called,
// including when Start fails, and only after Wait on a running child.
// interactive commands use their controlling terminal for prompts; background
// commands use the UI channel and never compete with Bubble Tea for terminal input.
func Command(ctx context.Context, args []string, cwd string, interactive bool) (*exec.Cmd, func(), error) {
	backend, err := executable()
	if err != nil {
		return nil, nil, err
	}
	return command(ctx, backend, args, cwd, interactive)
}

func command(ctx context.Context, backend string, args []string, cwd string, interactive bool) (*exec.Cmd, func(), error) {
	payload, err := json.Marshal(request{Args: args, Cwd: cwd, Prompt: !interactive})
	if err != nil || len(payload) > maxRequest {
		return nil, nil, errors.New("private request exceeds the 1 MiB limit")
	}
	read, write, err := os.Pipe()
	if err != nil {
		return nil, nil, err
	}
	cmd := exec.CommandContext(ctx, backend, "--request-fd")
	cmd.Args[0] = "fastjj"
	// cwd is deliberately supplied only in the request. No repository path is
	// placed in argv or environment, even for root discovery and workspace switches.
	cmd.ExtraFiles = []*os.File{read}
	done := make(chan struct{})
	var once sync.Once
	var parent, child *os.File
	cleanup := func() {
		once.Do(func() {
			close(done)
			read.Close()
			write.Close()
			if parent != nil {
				parent.Close()
			}
			if child != nil {
				child.Close()
			}
		})
	}
	if !interactive {
		syscall.ForkLock.RLock()
		sockets, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_STREAM, 0)
		if err != nil {
			syscall.ForkLock.RUnlock()
			cleanup()
			return nil, nil, err
		}
		unix.CloseOnExec(sockets[0])
		unix.CloseOnExec(sockets[1])
		syscall.ForkLock.RUnlock()
		parent = os.NewFile(uintptr(sockets[0]), "prompt-parent")
		child = os.NewFile(uintptr(sockets[1]), "prompt-child")
		cmd.ExtraFiles = append(cmd.ExtraFiles, child)
		promptState.RLock()
		handler := promptState.handler
		promptState.RUnlock()
		go servePrompts(parent, handler, done)
	}
	go func() {
		defer write.Close()
		_, _ = write.Write(payload)
	}()
	return cmd, cleanup, nil
}

func servePrompts(channel io.ReadWriteCloser, handler PromptHandler, done <-chan struct{}) {
	defer channel.Close()
	scanner := bufio.NewScanner(channel)
	scanner.Buffer(make([]byte, 4096), maxPrompt)
	for scanner.Scan() {
		var prompt struct {
			Prompt string `json:"prompt"`
			Secret bool   `json:"secret"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &prompt); err != nil {
			return
		}
		var answer []byte
		if handler != nil {
			answer = handler(prompt.Prompt, prompt.Secret, done)
		}
		response := struct {
			Value *string `json:"value"`
		}{}
		if answer != nil {
			text := string(answer)
			response.Value = &text
		}
		data, err := json.Marshal(response)
		clear(answer)
		if err != nil || len(data) > maxPrompt {
			clear(data)
			return
		}
		data = append(data, '\n')
		_, err = channel.Write(data)
		clear(data)
		if err != nil {
			return
		}
	}
}

// ShellError is shared by interactive shell and Lua entry points.
func ShellError() error {
	return fmt.Errorf("external shell commands are disabled by fastjjui's process privacy policy; use jj commands or Lua's jj API")
}
