package httpapi

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"balance-server/internal/config"
	"balance-server/internal/database"
)

func TestRegisterAndSynchronize(t *testing.T) {
	db, err := database.Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	cfg := config.Config{JWTSecret: "test-secret-that-is-longer-than-thirty-two-characters", AccessTTL: 15 * time.Minute, RefreshTTL: time.Hour, AllowRegistration: true}
	server := httptest.NewServer(New(cfg, db, slog.New(slog.NewTextHandler(io.Discard, nil))))
	defer server.Close()

	register := request(t, http.MethodPost, server.URL+"/v1/auth/register", map[string]any{"email": "person@example.com", "password": "correct horse battery staple"}, "")
	if register.StatusCode != http.StatusCreated {
		t.Fatalf("register status: %d", register.StatusCode)
	}
	var session sessionResponse
	decode(t, register, &session)
	if session.AccessToken == "" || session.RefreshToken == "" {
		t.Fatal("session tokens are empty")
	}

	now := time.Now().UTC().Add(-time.Second)
	pull := false
	syncBody := syncRequest{DeviceID: "test", Pull: &pull, Changes: []syncChange{{Entity: "category", ID: "category-1", UpdatedAt: now, Payload: json.RawMessage(`{"name":"Food"}`)}}}
	synchronized := request(t, http.MethodPost, server.URL+"/v1/sync", syncBody, session.AccessToken)
	if synchronized.StatusCode != http.StatusOK {
		t.Fatalf("sync status: %d", synchronized.StatusCode)
	}
	var result syncResponse
	decode(t, synchronized, &result)
	if result.Cursor != 0 || len(result.Changes) != 0 {
		t.Fatalf("push-only response included pulled changes: %+v", result)
	}

	pull = true
	synchronized = request(t, http.MethodPost, server.URL+"/v1/sync", syncRequest{DeviceID: "test", Pull: &pull}, session.AccessToken)
	if synchronized.StatusCode != http.StatusOK {
		t.Fatalf("pull status: %d", synchronized.StatusCode)
	}
	decode(t, synchronized, &result)
	if result.Cursor == 0 || len(result.Changes) != 1 || result.Changes[0].ID != "category-1" {
		t.Fatalf("unexpected sync response: %+v", result)
	}

	pageStart := result.Cursor
	pageChanges := make([]syncChange, syncPageSize+1)
	for index := range pageChanges {
		pageChanges[index] = syncChange{
			Entity: "category", ID: "page-" + time.Unix(int64(index), 0).UTC().Format("150405"),
			UpdatedAt: now.Add(time.Duration(index+1) * time.Millisecond), Payload: json.RawMessage(`{"name":"Paged"}`),
		}
	}
	pull = false
	synchronized = request(t, http.MethodPost, server.URL+"/v1/sync", syncRequest{DeviceID: "test", Pull: &pull, Changes: pageChanges}, session.AccessToken)
	if synchronized.StatusCode != http.StatusOK {
		t.Fatalf("paged push status: %d", synchronized.StatusCode)
	}
	synchronized.Body.Close()

	pull = true
	synchronized = request(t, http.MethodPost, server.URL+"/v1/sync", syncRequest{Cursor: pageStart, DeviceID: "test", Pull: &pull}, session.AccessToken)
	decode(t, synchronized, &result)
	if len(result.Changes) != syncPageSize || !result.HasMore {
		t.Fatalf("first page mismatch: count=%d hasMore=%v", len(result.Changes), result.HasMore)
	}
	synchronized = request(t, http.MethodPost, server.URL+"/v1/sync", syncRequest{Cursor: result.Cursor, DeviceID: "test", Pull: &pull}, session.AccessToken)
	decode(t, synchronized, &result)
	if len(result.Changes) != 1 || result.HasMore {
		t.Fatalf("last page mismatch: count=%d hasMore=%v", len(result.Changes), result.HasMore)
	}
}

func TestEmbeddedWebAppAndCookieSession(t *testing.T) {
	db, err := database.Open(filepath.Join(t.TempDir(), "web.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	cfg := config.Config{JWTSecret: "test-secret-that-is-longer-than-thirty-two-characters", AccessTTL: 15 * time.Minute, RefreshTTL: time.Hour, AllowRegistration: true}
	server := httptest.NewServer(New(cfg, db, slog.New(slog.NewTextHandler(io.Discard, nil))))
	defer server.Close()

	home, err := http.Get(server.URL + "/")
	if err != nil {
		t.Fatal(err)
	}
	homeBody, err := io.ReadAll(home.Body)
	home.Body.Close()
	if err != nil {
		t.Fatal(err)
	}
	if home.StatusCode != http.StatusOK || !strings.Contains(string(homeBody), "id=\"app-view\"") {
		t.Fatalf("web app response: status=%d body=%q", home.StatusCode, string(homeBody))
	}
	if !strings.Contains(home.Header.Get("Content-Security-Policy"), "script-src 'self'") {
		t.Fatal("web app is missing its content security policy")
	}

	register := request(t, http.MethodPost, server.URL+"/v1/auth/register", map[string]any{"email": "web@example.com", "password": "correct horse battery staple"}, "")
	if register.StatusCode != http.StatusCreated {
		t.Fatalf("register status: %d", register.StatusCode)
	}
	cookies := register.Cookies()
	var session sessionResponse
	decode(t, register, &session)
	var accessCookie, refreshCookie *http.Cookie
	for _, cookie := range cookies {
		switch cookie.Name {
		case accessCookieName:
			accessCookie = cookie
		case refreshCookieName:
			refreshCookie = cookie
		}
		if !cookie.HttpOnly || cookie.SameSite != http.SameSiteStrictMode {
			t.Fatalf("insecure session cookie: %+v", cookie)
		}
	}
	if accessCookie == nil || refreshCookie == nil {
		t.Fatal("session cookies were not returned")
	}

	meRequest, err := http.NewRequest(http.MethodGet, server.URL+"/v1/me", nil)
	if err != nil {
		t.Fatal(err)
	}
	meRequest.AddCookie(accessCookie)
	me, err := http.DefaultClient.Do(meRequest)
	if err != nil {
		t.Fatal(err)
	}
	if me.StatusCode != http.StatusOK {
		t.Fatalf("cookie authentication status: %d", me.StatusCode)
	}
	var user userResponse
	decode(t, me, &user)
	if user.Email != "web@example.com" {
		t.Fatalf("unexpected web user: %+v", user)
	}

	refreshData, err := json.Marshal(refreshRequest{})
	if err != nil {
		t.Fatal(err)
	}
	refreshHTTP, err := http.NewRequest(http.MethodPost, server.URL+"/v1/auth/refresh", bytes.NewReader(refreshData))
	if err != nil {
		t.Fatal(err)
	}
	refreshHTTP.Header.Set("Content-Type", "application/json")
	refreshHTTP.AddCookie(refreshCookie)
	refreshed, err := http.DefaultClient.Do(refreshHTTP)
	if err != nil {
		t.Fatal(err)
	}
	if refreshed.StatusCode != http.StatusOK {
		t.Fatalf("cookie refresh status: %d", refreshed.StatusCode)
	}
	refreshed.Body.Close()
}

func request(t *testing.T, method, url string, value any, token string) *http.Response {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	req, err := http.NewRequest(method, url, bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	return response
}

func decode(t *testing.T, response *http.Response, value any) {
	t.Helper()
	defer response.Body.Close()
	if err := json.NewDecoder(response.Body).Decode(value); err != nil {
		t.Fatal(err)
	}
}
