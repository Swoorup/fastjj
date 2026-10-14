package undo

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/idursun/jjui/internal/jj"
	"github.com/idursun/jjui/internal/ui/common"
	"github.com/idursun/jjui/internal/ui/intents"
	"github.com/idursun/jjui/internal/ui/layout"
	"github.com/idursun/jjui/internal/ui/render"
	"github.com/idursun/jjui/test"
	"github.com/stretchr/testify/assert"
)

func TestConfirm(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	commandRunner.Expect(jj.OpLog(1))
	commandRunner.Expect(jj.Undo())
	defer commandRunner.Verify()

	model := NewModel(test.NewTestContext(commandRunner))
	test.SimulateModel(model, model.Init())
	assert.Contains(t, test.RenderImmediate(model, 100, 20), "undo")

	test.SimulateModel(model, func() tea.Msg { return intents.Apply{} })
}

func TestCancel(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	commandRunner.Expect(jj.OpLog(1))
	defer commandRunner.Verify()

	model := NewModel(test.NewTestContext(commandRunner))
	test.SimulateModel(model, model.Init())
	assert.Contains(t, test.RenderImmediate(model, 100, 20), "undo")

	test.SimulateModel(model, func() tea.Msg { return intents.Cancel{} })
}

func TestOptionSelectMovesSelection(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	commandRunner.Expect(jj.OpLog(1))
	defer commandRunner.Verify()

	model := NewModel(test.NewTestContext(commandRunner))
	test.SimulateModel(model, model.Init())

	var msgs []tea.Msg
	test.SimulateModel(
		model,
		tea.Sequence(
			func() tea.Msg { return intents.OptionSelect{Delta: 1} },
			func() tea.Msg { return intents.Apply{} },
		),
		func(msg tea.Msg) {
			msgs = append(msgs, msg)
		},
	)

	assert.Contains(t, msgs, common.CloseViewMsg{}, "moving to the second option should close without running undo")
}

func TestUndo_ZIndex_RendersAbovePreview(t *testing.T) {
	commandRunner := test.NewTestCommandRunner(t)
	commandRunner.Expect(jj.OpLog(1))

	model := NewModel(test.NewTestContext(commandRunner))
	test.SimulateModel(model, model.Init())

	dl := render.NewDisplayContext()
	box := layout.Box{R: layout.Rect(0, 0, 100, 40)}
	dl.AddDraw(box.R, strings.Repeat("x", box.R.Dx()*box.R.Dy()), render.ZPreview)
	model.ViewRect(dl, box)

	rendered := dl.RenderToString(box.R.Dx(), box.R.Dy())
	assert.Contains(t, rendered, "undo", "undo confirmation should remain visible above preview content")
}
