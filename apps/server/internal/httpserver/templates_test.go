package httpserver

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"github.com/ZenNotes/zennotes/apps/server/internal/config"
	"github.com/ZenNotes/zennotes/apps/server/internal/vault"
)

func newTemplatesServer(t *testing.T) (*httptest.Server, string) {
	t.Helper()
	root := t.TempDir()
	cfg := config.Config{
		VaultPath:           root,
		Bind:                "127.0.0.1:0",
		AllowInsecureNoAuth: true,
	}
	v, err := vault.New(root, vault.Options{})
	if err != nil {
		t.Fatalf("vault.New: %v", err)
	}
	srv := httptest.NewServer(New(v, nil, nil, cfg).Router())
	t.Cleanup(srv.Close)
	return srv, root
}

func postJSON(t *testing.T, url string, body any) *http.Response {
	t.Helper()
	raw, err := json.Marshal(body)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	resp, err := http.Post(url, "application/json", bytes.NewReader(raw))
	if err != nil {
		t.Fatalf("post %s: %v", url, err)
	}
	return resp
}

func TestTemplatesListEmptyWithoutDir(t *testing.T) {
	srv, _ := newTemplatesServer(t)
	resp, err := http.Get(srv.URL + "/api/templates")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status: %d", resp.StatusCode)
	}
	var files []customTemplateFile
	if err := json.NewDecoder(resp.Body).Decode(&files); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(files) != 0 {
		t.Fatalf("expected no templates, got %d", len(files))
	}
}

func TestTemplatesWriteListReadDelete(t *testing.T) {
	srv, root := newTemplatesServer(t)

	resp := postJSON(t, srv.URL+"/api/templates/write", map[string]string{
		"slug": "Réunion Hebdo!",
		"raw":  "---\nname: Weekly\n---\n# {{title}}",
	})
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("write status: %d", resp.StatusCode)
	}
	var written customTemplateFile
	if err := json.NewDecoder(resp.Body).Decode(&written); err != nil {
		t.Fatalf("decode write: %v", err)
	}
	if written.SourcePath != ".zennotes/templates/r-union-hebdo.md" {
		t.Fatalf("slug normalization: got %q", written.SourcePath)
	}
	if _, err := os.Stat(filepath.Join(root, ".zennotes", "templates", "r-union-hebdo.md")); err != nil {
		t.Fatalf("template file missing on disk: %v", err)
	}

	list, err := http.Get(srv.URL + "/api/templates")
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	defer list.Body.Close()
	var files []customTemplateFile
	if err := json.NewDecoder(list.Body).Decode(&files); err != nil {
		t.Fatalf("decode list: %v", err)
	}
	if len(files) != 1 || files[0].SourcePath != written.SourcePath {
		t.Fatalf("list mismatch: %+v", files)
	}

	read, err := http.Get(srv.URL + "/api/templates/read?path=" + written.SourcePath)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	defer read.Body.Close()
	var got map[string]string
	if err := json.NewDecoder(read.Body).Decode(&got); err != nil {
		t.Fatalf("decode read: %v", err)
	}
	if got["raw"] != written.Raw {
		t.Fatalf("read raw mismatch: %q", got["raw"])
	}

	del := postJSON(t, srv.URL+"/api/templates/delete", map[string]string{
		"sourcePath": written.SourcePath,
	})
	defer del.Body.Close()
	if del.StatusCode != http.StatusOK {
		t.Fatalf("delete status: %d", del.StatusCode)
	}
	if _, err := os.Stat(filepath.Join(root, ".zennotes", "templates", "r-union-hebdo.md")); !os.IsNotExist(err) {
		t.Fatalf("template file should be gone, stat err: %v", err)
	}
}

func TestTemplatesWriteRenameRemovesPrevious(t *testing.T) {
	srv, root := newTemplatesServer(t)

	first := postJSON(t, srv.URL+"/api/templates/write", map[string]string{
		"slug": "adr", "raw": "v1",
	})
	first.Body.Close()

	renamed := postJSON(t, srv.URL+"/api/templates/write", map[string]string{
		"slug":               "decision-record",
		"raw":                "v2",
		"previousSourcePath": ".zennotes/templates/adr.md",
	})
	defer renamed.Body.Close()
	var out customTemplateFile
	if err := json.NewDecoder(renamed.Body).Decode(&out); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if out.SourcePath != ".zennotes/templates/decision-record.md" {
		t.Fatalf("renamed sourcePath: %q", out.SourcePath)
	}
	if _, err := os.Stat(filepath.Join(root, ".zennotes", "templates", "adr.md")); !os.IsNotExist(err) {
		t.Fatalf("previous file should be removed after rename")
	}
}

func TestTemplatesDuplicateSlugDeduplicates(t *testing.T) {
	srv, _ := newTemplatesServer(t)

	for range 2 {
		resp := postJSON(t, srv.URL+"/api/templates/write", map[string]string{
			"slug": "adr", "raw": "body",
		})
		resp.Body.Close()
	}
	list, err := http.Get(srv.URL + "/api/templates")
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	defer list.Body.Close()
	var files []customTemplateFile
	if err := json.NewDecoder(list.Body).Decode(&files); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(files) != 2 || files[0].SourcePath != ".zennotes/templates/adr-2.md" && files[1].SourcePath != ".zennotes/templates/adr-2.md" {
		t.Fatalf("expected adr.md + adr-2.md, got %+v", files)
	}
}

func TestTemplatesRejectPathsOutsideTemplatesDir(t *testing.T) {
	srv, _ := newTemplatesServer(t)

	for _, path := range []string{
		"../../etc/passwd",
		".zennotes/templates/../../notes.md",
		".zennotes/templates/sub/dir.md",
		".zennotes/templates/not-markdown.txt",
	} {
		resp, err := http.Get(srv.URL + "/api/templates/read?path=" + path)
		if err != nil {
			t.Fatalf("read %s: %v", path, err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("path %q: expected 400, got %d", path, resp.StatusCode)
		}
	}
}
