package context

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log"
	"os"
	"os/exec"
	"slices"
	"strings"
	"sync"
	"sync/atomic"

	tea "charm.land/bubbletea/v2"
	"github.com/idursun/jjui/internal/privateclient"
	"github.com/idursun/jjui/internal/ui/common"
)

type CommandRunner interface {
	RunCommandImmediate(args []string) ([]byte, error)
	RunCommandImmediateWithEnv(args []string, env []string) ([]byte, error)
	RunCommandStreaming(ctx context.Context, args []string) (*StreamingCommand, error)
	RunCommand(args []string, continuations ...tea.Cmd) tea.Cmd
	RunCommandWithInput(args []string, input string, continuations ...tea.Cmd) tea.Cmd
	RunInteractiveCommand(args []string, continuation tea.Cmd) tea.Cmd
}

type MainCommandRunner struct {
	Location  string
	idCounter atomic.Int64
}

func (a *MainCommandRunner) nextID() int { return int(a.idCounter.Add(1)) }

func (a *MainCommandRunner) RunCommandImmediateWithEnv(args []string, env []string) ([]byte, error) {
	c, cleanup, err := privateclient.Command(context.Background(), args, a.Location, false)
	if err != nil {
		return nil, err
	}
	defer cleanup()
	if len(env) > 0 {
		c.Env = append(os.Environ(), env...)
	}
	if output, err := c.Output(); err != nil {
		if exitError, ok := errors.AsType[*exec.ExitError](err); ok {
			return nil, errors.New(string(exitError.Stderr))
		}
		return nil, err
	} else {
		return bytes.Trim(output, "\n"), nil
	}
}

func (a *MainCommandRunner) RunCommandImmediate(args []string) ([]byte, error) {
	return a.RunCommandImmediateWithEnv(args, nil)
}

func (a *MainCommandRunner) RunCommandStreaming(ctx context.Context, args []string) (*StreamingCommand, error) {
	c, cleanup, err := privateclient.Command(ctx, args, a.Location, false)
	if err != nil {
		return nil, err
	}
	started := false
	defer func() {
		if !started {
			cleanup()
		}
	}()
	pipe, err := c.StdoutPipe()
	if err != nil {
		return nil, err
	}
	errPipe, err := c.StderrPipe()
	if err != nil {
		return nil, err
	}
	if err = c.Start(); err != nil {
		return nil, err
	}
	started = true
	return &StreamingCommand{
		cleanup:    cleanup,
		ReadCloser: pipe,
		ErrPipe:    errPipe,
		cmd:        c,
		ctx:        ctx,
	}, nil
}

func (a *MainCommandRunner) runCommandWithInput(args []string, input *string, continuations []tea.Cmd) tea.Cmd {
	id := a.nextID()
	command := "jj " + strings.Join(args, " ")
	commands := make([]tea.Cmd, 0)
	commands = append(commands,
		func() tea.Msg {
			if !slices.Contains(args, "--color") {
				args = append([]string{"--color", "always"}, args...)
			}
			c, cleanup, err := privateclient.Command(context.Background(), args, a.Location, false)
			if err != nil {
				return common.CommandCompletedMsg{ID: id, Err: err}
			}
			defer cleanup()

			if input != nil {
				stdin, err := c.StdinPipe()
				if err != nil {
					return common.CommandCompletedMsg{
						ID:  id,
						Err: err,
					}
				}
				go func() {
					defer stdin.Close()
					io.WriteString(stdin, *input)
				}()
			}

			var output bytes.Buffer
			c.Stderr = &output
			if err := c.Start(); err != nil {
				return common.CommandCompletedMsg{
					ID:  id,
					Err: err,
				}
			}

			err = c.Wait()
			if err != nil {
				if _, ok := errors.AsType[*exec.ExitError](err); ok {
					err = errors.New(output.String())
				}
			}
			return common.CommandCompletedMsg{
				ID:     id,
				Output: output.String(),
				Err:    err,
			}
		})
	commands = append(commands, continuations...)
	return tea.Batch(
		func() tea.Msg {
			return common.CommandRunningMsg{ID: id, Command: command}
		},
		tea.Sequence(commands...),
	)
}

func (a *MainCommandRunner) RunCommandWithInput(args []string, input string, continuations ...tea.Cmd) tea.Cmd {
	return a.runCommandWithInput(args, &input, continuations)
}

func (a *MainCommandRunner) RunCommand(args []string, continuations ...tea.Cmd) tea.Cmd {
	return a.runCommandWithInput(args, nil, continuations)
}

func (a *MainCommandRunner) RunInteractiveCommand(args []string, continuation tea.Cmd) tea.Cmd {
	return func() tea.Msg {
		c, cleanup, err := privateclient.Command(context.Background(), args, a.Location, true)
		if err != nil {
			return common.CommandCompletedMsg{Err: err}
		}
		// Preserve terminal IO for the native text/diff/merge editors.
		return tea.ExecProcess(c, func(err error) tea.Msg {
			cleanup()
			if err != nil {
				return common.CommandCompletedMsg{Err: err}
			}
			if continuation != nil {
				return continuation()
			}
			return nil
		})()
	}
}

type StreamingCommand struct {
	io.ReadCloser
	ErrPipe io.ReadCloser
	cmd     *exec.Cmd
	ctx     context.Context
	once    sync.Once
	cleanup func()
}

func (c *StreamingCommand) Close() error {
	var err error
	c.once.Do(func() {
		defer c.cleanup()
		log.Println("closing streaming command")
		pipeErr := c.ReadCloser.Close()

		if c.ctx.Err() != nil {
			log.Println("killing process due to context cancellation")
			if killErr := c.cmd.Process.Kill(); killErr != nil {
				err = killErr
				return
			}
		}

		log.Println("waiting for command to finish")
		err = c.cmd.Wait()
		if err != nil && (c.ctx.Err() != nil || errors.Is(err, os.ErrClosed)) {
			err = nil
		}

		if pipeErr != nil && err == nil {
			err = pipeErr
		}
	})
	return err
}

func (c *StreamingCommand) Wait() error {
	defer c.cleanup()
	return c.cmd.Wait()
}
