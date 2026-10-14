package actionargs

func StringArg(args map[string]any, name string, fallback string) string {
	if args == nil {
		return fallback
	}
	raw, ok := args[name]
	if !ok {
		return fallback
	}
	v, ok := raw.(string)
	if !ok {
		return fallback
	}
	return v
}
