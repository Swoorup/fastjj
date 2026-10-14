package intents

//jjui:bind scope=ui action=open_revset set=Clear:true
//jjui:bind scope=revset action=edit set=Clear:$bool(clear)
type Edit struct {
	Clear bool
}

func (Edit) isIntent() {}

//jjui:bind scope=revset action=set set=Value:$string(value)
type Set struct {
	Value string
}

func (Set) isIntent() {}

//jjui:bind scope=revset action=reset
type Reset struct{}

func (Reset) isIntent() {}

//jjui:bind scope=revset action=autocomplete
//jjui:bind scope=revset action=autocomplete_back set=Reverse:true
type CompletionCycle struct {
	Reverse bool
}

func (CompletionCycle) isIntent() {}

//jjui:bind scope=revset action=move_up set=Delta:-1
//jjui:bind scope=revset action=move_down set=Delta:1
type CompletionMove struct {
	Delta int
}

func (CompletionMove) isIntent() {}
