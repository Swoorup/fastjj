package autocompletion

type CompletionProvider interface {
	GetCompletions(input string) []string
	GetSignatureHelp(input string) string
	GetLastToken(input string) (int, string) // Returns the start index and text of the last token
}
