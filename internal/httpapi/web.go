package httpapi

import (
	"embed"
	"io/fs"
	"net/http"
	"path"
	"strings"
)

//go:embed web/*
var embeddedWeb embed.FS

var webContent = func() http.Handler {
	content, err := fs.Sub(embeddedWeb, "web")
	if err != nil {
		panic(err)
	}
	return http.FileServer(http.FS(content))
}()

func (api *API) webApp(w http.ResponseWriter, r *http.Request) {
	if strings.HasPrefix(r.URL.Path, "/v1/") {
		writeError(w, http.StatusNotFound, "not_found", "Endpoint not found")
		return
	}
	cleaned := path.Clean(r.URL.Path)
	if cleaned == "." || cleaned == "/" || cleaned == "/index.html" || cleaned == "/service-worker.js" {
		w.Header().Set("Cache-Control", "no-cache")
	} else {
		w.Header().Set("Cache-Control", "public, max-age=3600")
	}
	webContent.ServeHTTP(w, r)
}
