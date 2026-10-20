package preview

import (
	"errors"
	"slices"
	"testing"

	"github.com/idursun/jjui/internal/config"
	"github.com/idursun/jjui/internal/jj"
	"github.com/idursun/jjui/internal/ui/common"
	"github.com/idursun/jjui/internal/ui/intents"
	"github.com/idursun/jjui/internal/ui/layout"
	"github.com/idursun/jjui/internal/ui/render"

	"github.com/idursun/jjui/test"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type previewRunner struct {
	*test.CommandRunner
	calls int
	args  []string
	env   []string
	text  string
	err   error
}

func (r *previewRunner) RunCommandImmediateWithEnv(args, env []string) ([]byte, error) {
	r.calls++
	r.args, r.env = slices.Clone(args), slices.Clone(env)
	return []byte(r.text), r.err
}

func TestRefreshUpdatesSameRevisionAndRetainsScrollForUnchangedContent(t *testing.T) {
	runner := &previewRunner{CommandRunner: test.NewTestCommandRunner(t), text: "one\ntwo\nthree\nfour"}
	ctx := test.NewTestContext(runner)
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{
		Highlighted: common.SelectedRevision{ChangeId: "change", CommitId: "commit"},
	}})
	model := New(ctx)
	model.ViewRect(render.NewDisplayContext(), layout.NewBox(layout.Rect(0, 0, 80, 2)))
	test.SimulateModel(model, model.Update(common.SelectionChangedMsg{}))
	model.Scroll(1)
	test.SimulateModel(model, model.Update(common.RefreshMsg{}))
	assert.Equal(t, 2, runner.calls)
	assert.Equal(t, 1, model.YOffset())
	runner.text = "changed in the working copy"
	// A selection notification may supersede a refresh during its debounce.
	// It must not reuse the old output for the unchanged revision identifier.
	refresh := model.Update(common.RefreshMsg{})
	selection := model.Update(common.SelectionChangedMsg{})
	test.SimulateModel(model, refresh)
	test.SimulateModel(model, selection)
	assert.Equal(t, runner.text, model.content)
	assert.Equal(t, 3, runner.calls)
}

func TestResizeUsesNewPaneGeometryAndDoesNotReuseCachedOutput(t *testing.T) {
	runner := &previewRunner{CommandRunner: test.NewTestCommandRunner(t), text: "diff"}
	ctx := test.NewTestContext(runner)
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{
		Highlighted: common.SelectedRevision{ChangeId: "change", CommitId: "commit"},
	}})
	model := New(ctx)
	model.ViewRect(render.NewDisplayContext(), layout.NewBox(layout.Rect(0, 0, 80, 4)))
	test.SimulateModel(model, model.Update(common.SelectionChangedMsg{}))
	cmd := model.Update(ResizeMsg{})
	model.ViewRect(render.NewDisplayContext(), layout.NewBox(layout.Rect(0, 0, 42, 6)))
	test.SimulateModel(model, cmd)
	assert.Equal(t, 2, runner.calls)
	assert.Contains(t, runner.env, "COLUMNS=42")
	assert.Contains(t, runner.env, "DFT_WIDTH=42")
	assert.Contains(t, runner.env, "LINES=6")
}

func TestPreviewErrorsAreVisibleAndRetried(t *testing.T) {
	runner := &previewRunner{CommandRunner: test.NewTestCommandRunner(t), err: errors.New("Delta failed")}
	ctx := test.NewTestContext(runner)
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{
		Highlighted: common.SelectedRevision{ChangeId: "change", CommitId: "commit"},
	}})
	model := New(ctx)
	test.SimulateModel(model, model.Update(common.SelectionChangedMsg{}))
	assert.Contains(t, model.content, "Preview failed:\nDelta failed")
	runner.err, runner.text = nil, "recovered diff"
	test.SimulateModel(model, model.Update(common.SelectionChangedMsg{}))
	assert.Equal(t, "recovered diff", model.content)
	assert.Equal(t, 2, runner.calls)
}

func TestLatePreviewCannotReplaceNewSelection(t *testing.T) {
	runner := &previewRunner{CommandRunner: test.NewTestCommandRunner(t), text: "old"}
	ctx := test.NewTestContext(runner)
	old := common.SelectedRevision{ChangeId: "old", CommitId: "old"}
	newer := common.SelectedRevision{ChangeId: "new", CommitId: "new"}
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{Highlighted: old}})
	model := New(ctx)
	first := model.Update(common.SelectionChangedMsg{})
	oldCommand := model.Update(first())
	oldResult := oldCommand()
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{Highlighted: newer}})
	runner.text = "new"
	test.SimulateModel(model, model.Update(common.SelectionChangedMsg{}))
	model.Update(oldResult)
	assert.Equal(t, "new", model.content)
	assert.Equal(t, newer, model.contentItem)
}

type selectionStateProvider struct{ snapshot common.SelectionSnapshot }

func (p selectionStateProvider) Selection() common.SelectionSnapshot { return p.snapshot }
func (p selectionStateProvider) QueryState(string) (any, bool)       { return nil, false }

func TestModel_Init(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	defer commandRunner.Verify()

	ctx := test.NewTestContext(commandRunner)
	model := New(ctx)

	test.SimulateModel(model, model.Init())
}

func TestModel_View(t *testing.T) {
	tests := []struct {
		name     string
		scrollBy layout.Position
		atBottom bool
		width    int
		height   int
		content  string
		expected string
	}{
		{
			name:     "clips",
			scrollBy: layout.Position{},
			width:    5,
			height:   2,
			content: test.Stripped(`
			+++++..
			+abcde.
			+++++..
			`),
			expected: test.Stripped(`
			+++++
			+abcd
			`),
		},
		{
			name:     "clips when at bottom",
			scrollBy: layout.Position{},
			atBottom: true,
			width:    5,
			height:   3,
			content: test.Stripped(`
			+++++..
			+abcde.
			+++++..
			`),
			expected: test.Stripped(`
			+++++
			+abcd
			+++++
			`),
		},
		{
			name:     "Scroll by down and right",
			scrollBy: layout.Position{X: 1, Y: 1},
			width:    5,
			height:   2,
			content: test.Stripped(`
			.......
			.abcde.
			.......
			`),
			expected: test.Stripped(`
			abcde
			.....
			`),
		},
		{
			name:     "Scroll down when at bottom",
			scrollBy: layout.Position{X: 0, Y: 1},
			atBottom: true,
			width:    5,
			height:   3,
			content: test.Stripped(`
			.......
			.abcde.
			.......
			`),
			expected: test.Stripped(`
			.abcd
			.....
			`),
		},
		{
			name:     "Scroll 2 right when at bottom",
			scrollBy: layout.Position{X: 2, Y: 0},
			atBottom: true,
			width:    5,
			height:   3,
			content: test.Stripped(`
			.......
			.abcde.
			.......
			`),
			expected: test.Stripped(`
			.....
			bcde.
			.....
			`),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ctx := test.NewTestContext(test.NewTestCommandRunner(t))

			model := New(ctx)

			model.SetContent(tc.content)
			if tc.scrollBy.X > 0 {
				model.ScrollHorizontal(tc.scrollBy.X)
			}
			if tc.scrollBy.Y > 0 {
				model.Scroll(tc.scrollBy.Y)
			}
			v := test.Stripped(test.RenderImmediate(model, tc.width, tc.height))

			assert.Equal(t, tc.expected, v)
		})
	}
}

func TestUpdate_PreviewShowResetsScroll(t *testing.T) {
	ctx := test.NewTestContext(test.NewTestCommandRunner(t))
	model := New(ctx)

	model.SetContent("first line\nsecond line\nthird line")
	model.Scroll(2)
	model.ScrollHorizontal(2)

	cmd := model.Update(intents.PreviewShow{Content: "updated"})
	require.Nil(t, cmd)

	assert.Equal(t, 0, model.view.YOffset())
	assert.Equal(t, 0, model.view.XOffset())
	assert.Equal(t, "updated", model.content)
}

func TestUpdate_PreviewShowDoesNotBreakSelectionRefresh(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	defer commandRunner.Verify()

	ctx := test.NewTestContext(commandRunner)
	ctx.CurrentRevset = "all()"
	model := New(ctx)
	model.ViewRect(render.NewDisplayContext(), layout.NewBox(layout.Rect(0, 0, 80, 3)))

	selected := common.SelectedRevision{ChangeId: "change", CommitId: "commit"}
	ctx.SetStateProvider(selectionStateProvider{snapshot: common.SelectionSnapshot{Highlighted: selected}})
	args := jj.TemplatedArgs(config.Current.Preview.RevisionCommand, map[string]string{
		jj.RevsetPlaceholder:       ctx.CurrentRevset,
		jj.ChangeIdPlaceholder:     selected.ChangeId,
		jj.CommitIdPlaceholder:     selected.CommitId,
		jj.PreviewWidthPlaceholder: "80",
	})
	commandRunner.Expect(args).SetOutput([]byte("line 1\nline 2\nline 3\nline 4\nline 5"))

	model.Update(intents.PreviewShow{Content: "manual"})
	cmd := model.Update(common.RefreshMsg{})
	require.NotNil(t, cmd)

	test.SimulateModel(model, cmd)

	assert.Equal(t, "line 1\nline 2\nline 3\nline 4\nline 5", model.content)

	model.Scroll(2)
	assert.Equal(t, 2, model.YOffset())

	cmd = model.Update(common.RefreshMsg{})
	require.NotNil(t, cmd)

	test.SimulateModel(model, cmd)

	assert.Equal(t, "line 1\nline 2\nline 3\nline 4\nline 5", model.content)
	assert.Equal(t, 2, model.YOffset())
}

func TestSetContent_ExpandsTabsUsingTabStops(t *testing.T) {
	ctx := test.NewTestContext(test.NewTestCommandRunner(t))
	model := New(ctx)

	model.SetContent("+\tfoo")

	rendered := test.Stripped(test.RenderImmediate(model, 12, 1))
	assert.Equal(t, "+   foo", rendered)
}

func TestSetContent_ResetsTabStopsAfterNewlines(t *testing.T) {
	ctx := test.NewTestContext(test.NewTestCommandRunner(t))
	model := New(ctx)

	model.SetContent("a\tb\nab\tc")

	rendered := test.Stripped(test.RenderImmediate(model, 12, 2))
	assert.Equal(t, "a   b\nab  c", rendered)
}
