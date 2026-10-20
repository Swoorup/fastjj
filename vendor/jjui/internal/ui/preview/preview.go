package preview

import (
	"slices"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"github.com/idursun/jjui/internal/config"
	"github.com/idursun/jjui/internal/jj"
	"github.com/idursun/jjui/internal/ui/actions"
	"github.com/idursun/jjui/internal/ui/common"
	"github.com/idursun/jjui/internal/ui/context"
	"github.com/idursun/jjui/internal/ui/intents"
	"github.com/idursun/jjui/internal/ui/layout"
	"github.com/idursun/jjui/internal/ui/render"
	"github.com/idursun/jjui/internal/ui/theme"
)

var _ common.ImmediateModel = (*Model)(nil)

type Model struct {
	view        viewport.Model
	content     string
	contentItem common.SelectedItem
	context     *context.MainContext
	generation  uint64
	contentArgs []string
	contentEnv  []string
	dirty       bool
}

const (
	debounceId       = "preview-refresh"
	debounceDuration = 50 * time.Millisecond
)

type previewMsg struct {
	msg tea.Msg
}

type updatePreviewContentMsg struct {
	contentItem common.SelectedItem
	content     string
	generation  uint64
	args        []string
	env         []string
	err         error
}

// ResizeMsg requests rendering after the next layout pass has sized the pane.
type ResizeMsg struct{}

type refreshPreviewMsg struct {
	item       common.SelectedItem
	force      bool
	generation uint64
}

type ScrollMsg struct {
	Delta      int
	Horizontal bool
}

func (s ScrollMsg) SetDelta(delta int, horizontal bool) tea.Msg {
	s.Delta = delta
	s.Horizontal = horizontal
	return s
}

func (m *Model) Scopes() []common.Scope {
	return []common.Scope{
		{
			Name:    actions.ScopeUiPreview,
			Leak:    common.LeakAll,
			Global:  true,
			Handler: m,
		},
	}
}

func (m *Model) HandleIntent(intent intents.Intent) (tea.Cmd, bool) {
	switch msg := intent.(type) {
	case intents.PreviewScroll:
		switch msg.Kind {
		case intents.PreviewScrollUp:
			return m.Scroll(-1), true
		case intents.PreviewScrollDown:
			return m.Scroll(1), true
		case intents.PreviewPageUp:
			return m.PageUp(), true
		case intents.PreviewPageDown:
			return m.PageDown(), true
		case intents.PreviewHalfPageUp:
			return m.HalfPageUp(), true
		case intents.PreviewHalfPageDown:
			return m.HalfPageDown(), true
		}
		return nil, true
	}
	return nil, false
}

func (m *Model) Init() tea.Cmd {
	return nil
}

func (m *Model) OnShow() tea.Cmd {
	m.reset()
	m.generation++
	m.contentItem = nil
	return nil
}

func (m *Model) OnHide() { m.generation++ }

func (m *Model) SetFocused(bool) {}

func (m *Model) YOffset() int {
	return m.view.YOffset()
}

func (m *Model) Scroll(delta int) tea.Cmd {
	if delta > 0 {
		m.view.ScrollDown(delta)
	} else if delta < 0 {
		m.view.ScrollUp(-delta)
	}
	return nil
}

func (m *Model) ScrollHorizontal(delta int) tea.Cmd {
	if delta > 0 {
		m.view.ScrollRight(delta)
	} else if delta < 0 {
		m.view.ScrollLeft(-delta)
	}

	return nil
}

func (m *Model) HalfPageDown() tea.Cmd {
	m.view.HalfPageDown()
	return nil
}

func (m *Model) HalfPageUp() tea.Cmd {
	m.view.HalfPageUp()
	return nil
}

func (m *Model) PageDown() tea.Cmd {
	m.view.PageDown()
	return nil
}

func (m *Model) PageUp() tea.Cmd {
	m.view.PageUp()
	return nil
}

func (m *Model) Update(msg tea.Msg) tea.Cmd {
	if k, ok := msg.(previewMsg); ok {
		msg = k.msg
	}
	switch msg := msg.(type) {
	case ScrollMsg:
		if msg.Horizontal {
			m.ScrollHorizontal(msg.Delta)
		} else {
			m.Scroll(msg.Delta)
		}
	case intents.PreviewShow:
		m.generation++
		m.contentItem = nil
		m.SetContent(msg.Content)
		return nil
	case common.SelectionChangedMsg:
		return m.refreshPreviewForItem(m.context.Selection().Highlighted, false)
	case common.RefreshMsg:
		return m.refreshPreviewForItem(m.context.Selection().Highlighted, true)
	case tea.WindowSizeMsg, ResizeMsg:
		return m.refreshPreviewForItem(m.context.Selection().Highlighted, false)
	case refreshPreviewMsg:
		if msg.generation != m.generation {
			return nil
		}
		return m.runPreview(msg)
	case updatePreviewContentMsg:
		if msg.generation != m.generation {
			return nil
		}
		if msg.err != nil {
			m.contentItem = nil // allow retry after configuration/tool errors
			m.SetContent("Preview failed:\n" + msg.err.Error())
			return nil
		}
		sameItem := m.contentItem != nil && m.contentItem.Equal(msg.contentItem)
		m.contentItem = msg.contentItem
		m.contentArgs, m.contentEnv = msg.args, msg.env
		m.dirty = false
		content := normalizeContent(msg.content)
		if content != m.content || !sameItem {
			m.SetContent(content)
		}
		return nil
	}
	return nil
}

func (m *Model) SetContent(content string) {
	content = normalizeContent(content)
	m.reset()
	m.content = content
	m.view.SetContent(content)
}

func normalizeContent(content string) string {
	content = strings.ReplaceAll(content, "\r", "")
	if strings.ContainsRune(content, '\t') {
		lines := strings.Split(content, "\n")
		for i, line := range lines {
			lines[i] = render.ExpandTabs(line)
		}
		content = strings.Join(lines, "\n")
	}
	return content
}

func (m *Model) ViewRect(dl *render.DisplayContext, box layout.Box) {
	surfaceStyle := theme.DefaultPalette.Get("preview", "", "", false)
	m.view.SetWidth(box.R.Dx())
	m.view.SetHeight(box.R.Dy())
	dl.AddFill(box.R, ' ', surfaceStyle, render.ZPreview)
	dl.AddDraw(box.R, m.view.View(), render.ZPreview, render.PreserveBackground())

	scrollRect := layout.Rect(box.R.Min.X, box.R.Min.Y, box.R.Dx(), box.R.Dy())
	dl.AddInteraction(scrollRect, ScrollMsg{}, render.InteractionScroll, render.ZPreview)
}

func (m *Model) reset() {
	m.view.SetYOffset(0)
	m.view.SetXOffset(0)
}

func (m *Model) refreshPreviewForItem(item common.SelectedItem, force bool) tea.Cmd {
	m.generation++
	m.dirty = m.dirty || force
	request := refreshPreviewMsg{item: item, force: force, generation: m.generation}
	// Return a message first: read pane geometry and configuration on the UI
	// thread, after layout, rather than racing ViewRect from a command goroutine.
	return common.Debounce(debounceId, debounceDuration, func() tea.Msg { return request })
}

func (m *Model) runPreview(request refreshPreviewMsg) tea.Cmd {
	item := request.item
	var args []string
	previewWidth := strconv.Itoa(m.view.Width())
	switch sel := item.(type) {
	case common.SelectedFile:
		args = jj.TemplatedArgs(config.Current.Preview.FileCommand, map[string]string{
			jj.RevsetPlaceholder:       m.context.CurrentRevset,
			jj.ChangeIdPlaceholder:     sel.ChangeId,
			jj.CommitIdPlaceholder:     sel.CommitId,
			jj.FilePlaceholder:         sel.File.Path(),
			jj.PreviewWidthPlaceholder: previewWidth,
		})
	case common.SelectedRevision:
		args = jj.TemplatedArgs(config.Current.Preview.RevisionCommand, map[string]string{
			jj.RevsetPlaceholder:       m.context.CurrentRevset,
			jj.ChangeIdPlaceholder:     sel.ChangeId,
			jj.CommitIdPlaceholder:     sel.CommitId,
			jj.PreviewWidthPlaceholder: previewWidth,
		})
	case common.SelectedCommit:
		args = jj.TemplatedArgs(config.Current.Preview.EvologCommand, map[string]string{
			jj.RevsetPlaceholder:       m.context.CurrentRevset,
			jj.CommitIdPlaceholder:     sel.CommitId,
			jj.PreviewWidthPlaceholder: previewWidth,
		})
	case common.SelectedOperation:
		args = jj.TemplatedArgs(config.Current.Preview.OplogCommand, map[string]string{
			jj.RevsetPlaceholder:       m.context.CurrentRevset,
			jj.OperationIdPlaceholder:  sel.OperationId,
			jj.PreviewWidthPlaceholder: previewWidth,
		})
	default:
		m.contentItem = nil
		m.SetContent("")
		return nil
	}

	env := []string{
		// The preview subprocess does not run in a pane-sized PTY, so let
		// width-sensitive tools like `jj diff` see the preview size via the
		// conventional terminal size environment variables.
		"DFT_WIDTH=" + strconv.Itoa(m.view.Width()), // difftastic
		"COLUMNS=" + strconv.Itoa(m.view.Width()),
		"LINES=" + strconv.Itoa(m.view.Height()),
	}
	if !m.dirty && !request.force && m.contentItem != nil && m.contentItem.Equal(item) &&
		slices.Equal(args, m.contentArgs) && slices.Equal(env, m.contentEnv) {
		return nil
	}
	runner := m.context.CommandRunner
	return func() tea.Msg {
		output, err := runner.RunCommandImmediateWithEnv(args, env)
		return updatePreviewContentMsg{
			contentItem: item,
			content:     string(output),
			generation:  request.generation,
			args:        args,
			env:         env,
			err:         err,
		}
	}
}

func New(context *context.MainContext) *Model {
	return &Model{
		context: context,
	}
}
