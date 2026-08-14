package httpserver

import (
	"io"
	"net/http"
	"strings"
	"time"
)

// tikzRender proxies TikZ render requests to a local render process (a small
// Node sidecar hosting node-tikzjax's wasm TeX engine — Go cannot run it).
// The upstream owns caching and render serialization; this is a dumb pipe.
func (s *Server) tikzRender(w http.ResponseWriter, r *http.Request) {
	upstream := strings.TrimSpace(s.currentConfig().TikzUpstream)
	if upstream == "" {
		writeJSON(w, http.StatusOK, map[string]any{
			"ok":    false,
			"error": "TikZ rendering is not configured on this server.",
		})
		return
	}
	// First render loads a ~5MB wasm TeX engine upstream; be generous.
	client := &http.Client{Timeout: 120 * time.Second}
	resp, err := client.Post(strings.TrimRight(upstream, "/")+"/render", "application/json", r.Body)
	if err != nil {
		writeJSON(w, http.StatusOK, map[string]any{
			"ok":    false,
			"error": "TikZ render process is unreachable: " + err.Error(),
		})
		return
	}
	defer resp.Body.Close()
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(resp.StatusCode)
	_, _ = io.Copy(w, resp.Body)
}
