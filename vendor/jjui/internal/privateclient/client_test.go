package privateclient

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"
)

// The helper is this test binary, launched through the production descriptor
// plumbing. Its argv remains exactly the same as a real backend's argv.
func TestMain(m *testing.M) {
	if os.Getenv("FASTJJ_PRIVATE_HELPER") == "1" {
		var req request
		data, err := io.ReadAll(os.NewFile(3, "request"))
		if err != nil {
			panic(err)
		}
		if err := json.Unmarshal(data, &req); err != nil {
			panic(err)
		}
		input, _ := io.ReadAll(os.Stdin)
		var answer map[string]any
		if req.Prompt {
			socket := os.NewFile(4, "prompt")
			_, _ = io.WriteString(socket, "{\"prompt\":\"KEY_CANARY\",\"secret\":true}\n")
			if err := json.NewDecoder(socket).Decode(&answer); err != nil {
				panic(err)
			}
		}
		_ = json.NewEncoder(os.Stdout).Encode(map[string]any{"args": os.Args, "request": req, "stdin": string(input), "answer": answer})
		os.Exit(0)
	}
	os.Exit(m.Run())
}

func TestPrivateDescriptorsPreserveStdinAndPrompt(t *testing.T) {
	self, _ := os.Executable()
	SetPromptHandler(func(prompt string, secret bool, done <-chan struct{}) []byte {
		if prompt != "KEY_CANARY" || !secret {
			t.Error("incorrect prompt")
		}
		return []byte("PASSPHRASE_CANARY λ")
	})
	defer SetPromptHandler(nil)
	for _, interactive := range []bool{false, true} {
		// Exceeds a pipe buffer, exercising asynchronous request writing.
		args := []string{"describe", "--stdin", strings.Repeat("MESSAGE_CANARY", 10000)}
		cmd, cleanup, err := command(context.Background(), self, args, "/REPOSITORY_CANARY", interactive)
		if err != nil {
			t.Fatal(err)
		}
		cmd.Env = append(os.Environ(), "FASTJJ_PRIVATE_HELPER=1")
		cmd.Stdin = strings.NewReader("description from stdin λ\n")
		if !reflect.DeepEqual(cmd.Args, []string{"fjj", "--request-fd"}) {
			t.Fatal(cmd.Args)
		}
		output, err := cmd.Output()
		cleanup()
		cleanup()
		if err != nil {
			t.Fatal(err)
		}
		var got struct {
			Args    []string
			Request request
			Stdin   string
			Answer  map[string]any
		}
		if err := json.Unmarshal(output, &got); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(got.Args, cmd.Args) || !reflect.DeepEqual(got.Request.Args, args) || got.Request.Cwd != "/REPOSITORY_CANARY" || got.Stdin != "description from stdin λ\n" {
			t.Fatal("descriptor data corrupted")
		}
		if !interactive && got.Answer["value"] != "PASSPHRASE_CANARY λ" {
			t.Fatal("credential response corrupted")
		}
	}
}

func TestPromptCancellationAndMalformedFrames(t *testing.T) {
	for _, input := range []string{"{\"prompt\":\"cancel\",\"secret\":true}\n", "invalid\n", strings.Repeat("x", maxPrompt+1) + "\n"} {
		server, client := net.Pipe()
		done := make(chan struct{})
		go servePrompts(server, nil, done)
		_ = client.SetDeadline(time.Now().Add(3 * time.Second))
		go func() { _, _ = io.WriteString(client, input) }()
		result, err := bufio.NewReader(client).ReadString('\n')
		if strings.HasPrefix(input, "{") {
			if err != nil || result != "{\"value\":null}\n" {
				t.Fatalf("cancellation: %q %v", result, err)
			}
		} else if err == nil {
			t.Fatal("malformed frame accepted")
		}
		client.Close()
		close(done)
	}
}

func TestRequestLimitAndStartFailure(t *testing.T) {
	if _, _, err := command(context.Background(), "missing", []string{strings.Repeat("x", maxRequest)}, "", false); err == nil {
		t.Fatal("oversized request accepted")
	}
	cmd, cleanup, err := command(context.Background(), "/nonexistent-fjj", []string{"status"}, "", false)
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err == nil {
		t.Fatal("unexpected start")
	}
	cleanup()
}
