package httpserver

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/ZenNotes/zennotes/apps/server/internal/vault"
)

// Custom-template file I/O, mirroring the desktop module
// (apps/desktop/src/main/templates.ts). Templates are plain `.md` files in
// the flat `.zennotes/templates/` directory; the server only moves raw bytes
// and the client owns all frontmatter parsing.

const templatesRelDir = ".zennotes/templates"

// customTemplateFile matches bridge-contract's CustomTemplateFile.
type customTemplateFile struct {
	SourcePath string `json:"sourcePath"`
	Raw        string `json:"raw"`
}

type writeTemplateInput struct {
	Slug               string `json:"slug"`
	Raw                string `json:"raw"`
	PreviousSourcePath string `json:"previousSourcePath"`
}

func templatesDir(root string) string {
	return filepath.Join(root, ".zennotes", "templates")
}

// resolveTemplatePath resolves a vault-relative sourcePath, rejecting
// anything outside the flat templates directory (no traversal, no
// subdirectories) and non-`.md` files.
func resolveTemplatePath(root, sourcePath string) (string, error) {
	abs, err := vault.SafeJoin(root, sourcePath)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(templatesDir(root), abs)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || strings.ContainsRune(rel, filepath.Separator) {
		return "", httpStatusError{code: http.StatusBadRequest, msg: fmt.Sprintf("refusing template path outside templates dir: %s", sourcePath)}
	}
	if !strings.HasSuffix(strings.ToLower(abs), ".md") {
		return "", httpStatusError{code: http.StatusBadRequest, msg: fmt.Sprintf("template path must be a .md file: %s", sourcePath)}
	}
	return abs, nil
}

// safeTemplateSlug: lowercase letters, digits, dashes; no separators.
func safeTemplateSlug(slug string) string {
	var b strings.Builder
	lastDash := true // trims leading dashes
	for _, r := range strings.ToLower(slug) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') || r == '-' {
			if r == '-' && lastDash {
				continue
			}
			b.WriteRune(r)
			lastDash = r == '-'
		} else if !lastDash {
			b.WriteRune('-')
			lastDash = true
		}
	}
	cleaned := strings.TrimRight(b.String(), "-")
	if cleaned == "" {
		return "template"
	}
	return cleaned
}

func templateFilenameStem(sourcePath string) string {
	parts := strings.Split(sourcePath, "/")
	file := parts[len(parts)-1]
	return strings.TrimSuffix(strings.TrimSuffix(file, ".md"), ".MD")
}

// uniqueTemplateSlug picks a free slug: overwriting the file being edited is
// allowed, otherwise de-duplicate against existing files (adr → adr-2 → …).
func uniqueTemplateSlug(dir, base, previousSourcePath string) string {
	prevStem := ""
	if previousSourcePath != "" {
		prevStem = templateFilenameStem(previousSourcePath)
	}
	candidate := base
	for n := 2; ; n++ {
		if candidate == prevStem {
			return candidate
		}
		if _, err := os.Stat(filepath.Join(dir, candidate+".md")); os.IsNotExist(err) {
			return candidate
		}
		candidate = fmt.Sprintf("%s-%d", base, n)
	}
}

func (s *Server) listTemplates(w http.ResponseWriter, _ *http.Request) {
	dir := templatesDir(s.currentVault().Root())
	entries, err := os.ReadDir(dir)
	if err != nil {
		writeJSON(w, http.StatusOK, []customTemplateFile{}) // no templates dir yet
		return
	}
	files := []customTemplateFile{}
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || strings.HasPrefix(name, ".") || !strings.HasSuffix(strings.ToLower(name), ".md") {
			continue
		}
		raw, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			continue // skip unreadable, like the desktop does
		}
		files = append(files, customTemplateFile{SourcePath: templatesRelDir + "/" + name, Raw: string(raw)})
	}
	sort.Slice(files, func(i, j int) bool { return files[i].SourcePath < files[j].SourcePath })
	writeJSON(w, http.StatusOK, files)
}

func (s *Server) readTemplate(w http.ResponseWriter, r *http.Request) {
	abs, err := resolveTemplatePath(s.currentVault().Root(), r.URL.Query().Get("path"))
	if err != nil {
		writeError(w, err)
		return
	}
	raw, err := os.ReadFile(abs)
	if err != nil {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"raw": string(raw)})
}

func (s *Server) writeTemplate(w http.ResponseWriter, r *http.Request) {
	var input writeTemplateInput
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		writeError(w, httpStatusError{code: http.StatusBadRequest, msg: "invalid JSON body"})
		return
	}
	root := s.currentVault().Root()
	dir := templatesDir(root)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		writeError(w, err)
		return
	}
	slug := uniqueTemplateSlug(dir, safeTemplateSlug(input.Slug), input.PreviousSourcePath)
	abs := filepath.Join(dir, slug+".md")
	if err := os.WriteFile(abs, []byte(input.Raw), 0o644); err != nil {
		writeError(w, err)
		return
	}
	// Renaming during an edit: remove the prior file if the slug changed.
	if input.PreviousSourcePath != "" {
		prevAbs, err := resolveTemplatePath(root, input.PreviousSourcePath)
		if err == nil && prevAbs != abs {
			_ = os.Remove(prevAbs)
		}
	}
	writeJSON(w, http.StatusOK, customTemplateFile{SourcePath: templatesRelDir + "/" + slug + ".md", Raw: input.Raw})
}

func (s *Server) deleteTemplate(w http.ResponseWriter, r *http.Request) {
	var input struct {
		SourcePath string `json:"sourcePath"`
	}
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		writeError(w, httpStatusError{code: http.StatusBadRequest, msg: "invalid JSON body"})
		return
	}
	abs, err := resolveTemplatePath(s.currentVault().Root(), input.SourcePath)
	if err != nil {
		writeError(w, err)
		return
	}
	if err := os.Remove(abs); err != nil && !os.IsNotExist(err) {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]bool{"ok": true})
}
