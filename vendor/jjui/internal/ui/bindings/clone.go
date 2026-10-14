package bindings

import "maps"

// CloneArgs returns a shallow copy of argument map or nil for empty input.
func CloneArgs(src map[string]any) map[string]any {
	if len(src) == 0 {
		return nil
	}
	out := make(map[string]any, len(src))
	maps.Copy(out, src)
	return out
}
