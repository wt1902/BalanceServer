package httpapi

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"net/http"
	"strings"
	"time"

	"balance-server/internal/auth"
)

type credentials struct {
	Email    string `json:"email"`
	Password string `json:"password"`
}
type refreshRequest struct {
	RefreshToken string `json:"refreshToken"`
}
type userResponse struct {
	ID    string `json:"id"`
	Email string `json:"email"`
}
type sessionResponse struct {
	AccessToken  string       `json:"accessToken"`
	RefreshToken string       `json:"refreshToken"`
	ExpiresAt    time.Time    `json:"expiresAt"`
	User         userResponse `json:"user"`
}

func (api *API) register(w http.ResponseWriter, r *http.Request) {
	if !api.cfg.AllowRegistration {
		writeError(w, http.StatusForbidden, "registration_disabled", "Registration is disabled")
		return
	}
	var request credentials
	if !decodeJSON(w, r, &request) {
		return
	}
	email, err := normalizeEmail(request.Email)
	if err != nil {
		writeError(w, http.StatusBadRequest, "invalid_email", "Enter a valid email address")
		return
	}
	passwordHash, err := auth.HashPassword(request.Password)
	if err != nil {
		writeError(w, http.StatusBadRequest, "invalid_password", err.Error())
		return
	}
	userID, err := identifier()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not create account")
		return
	}
	_, err = api.db.ExecContext(r.Context(), `INSERT INTO users(id,email,password_hash,created_at) VALUES(?,?,?,?)`, userID, email, passwordHash, time.Now().UTC().Format(time.RFC3339Nano))
	if err != nil {
		if containsUnique(err) {
			writeError(w, http.StatusConflict, "email_exists", "An account with this email already exists")
			return
		}
		api.logger.Error("create user", "error", err)
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not create account")
		return
	}
	response, err := api.createSession(r, userID, email)
	if err != nil {
		api.logger.Error("create session", "error", err)
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not create session")
		return
	}
	api.setSessionCookies(w, r, response)
	writeJSON(w, http.StatusCreated, response)
}

func (api *API) login(w http.ResponseWriter, r *http.Request) {
	var request credentials
	if !decodeJSON(w, r, &request) {
		return
	}
	email, err := normalizeEmail(request.Email)
	if err != nil {
		writeError(w, http.StatusUnauthorized, "invalid_credentials", "Email or password is incorrect")
		return
	}
	var userID, passwordHash string
	err = api.db.QueryRowContext(r.Context(), `SELECT id,password_hash FROM users WHERE email=?`, email).Scan(&userID, &passwordHash)
	if err != nil || !auth.VerifyPassword(passwordHash, request.Password) {
		writeError(w, http.StatusUnauthorized, "invalid_credentials", "Email or password is incorrect")
		return
	}
	response, err := api.createSession(r, userID, email)
	if err != nil {
		api.logger.Error("create session", "error", err)
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not create session")
		return
	}
	api.setSessionCookies(w, r, response)
	writeJSON(w, http.StatusOK, response)
}

func (api *API) refresh(w http.ResponseWriter, r *http.Request) {
	var request refreshRequest
	if !decodeJSON(w, r, &request) {
		return
	}
	request.RefreshToken = api.refreshToken(r, request.RefreshToken)
	if request.RefreshToken == "" {
		writeError(w, http.StatusBadRequest, "refresh_token_required", "Refresh token is required")
		return
	}
	tx, err := api.db.BeginTx(r.Context(), nil)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not refresh session")
		return
	}
	defer tx.Rollback()
	var userID, email, expiresText string
	err = tx.QueryRowContext(r.Context(), `SELECT u.id,u.email,t.expires_at FROM refresh_tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=? AND t.revoked_at IS NULL`, auth.TokenHash(request.RefreshToken)).Scan(&userID, &email, &expiresText)
	if err != nil {
		writeError(w, http.StatusUnauthorized, "invalid_refresh_token", "Refresh token is invalid or expired")
		return
	}
	expires, err := time.Parse(time.RFC3339Nano, expiresText)
	if err != nil || !time.Now().UTC().Before(expires) {
		writeError(w, http.StatusUnauthorized, "invalid_refresh_token", "Refresh token is invalid or expired")
		return
	}
	_, err = tx.ExecContext(r.Context(), `UPDATE refresh_tokens SET revoked_at=? WHERE token_hash=?`, time.Now().UTC().Format(time.RFC3339Nano), auth.TokenHash(request.RefreshToken))
	if err != nil {
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not refresh session")
		return
	}
	response, err := api.createSessionWithExecutor(r, userID, email, tx)
	if err != nil || tx.Commit() != nil {
		writeError(w, http.StatusInternalServerError, "internal_error", "Could not refresh session")
		return
	}
	api.setSessionCookies(w, r, response)
	writeJSON(w, http.StatusOK, response)
}

func (api *API) logout(w http.ResponseWriter, r *http.Request) {
	var request refreshRequest
	if !decodeJSON(w, r, &request) {
		return
	}
	request.RefreshToken = api.refreshToken(r, request.RefreshToken)
	if request.RefreshToken != "" {
		_, _ = api.db.ExecContext(r.Context(), `UPDATE refresh_tokens SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL`, time.Now().UTC().Format(time.RFC3339Nano), auth.TokenHash(request.RefreshToken))
	}
	api.clearSessionCookies(w, r)
	w.WriteHeader(http.StatusNoContent)
}

func (api *API) me(w http.ResponseWriter, r *http.Request) {
	value := claims(r)
	writeJSON(w, http.StatusOK, userResponse{ID: value.Subject, Email: value.Email})
}

func (api *API) createSession(r *http.Request, userID, email string) (sessionResponse, error) {
	return api.createSessionWithExecutor(r, userID, email, api.db)
}

func (api *API) createSessionWithExecutor(r *http.Request, userID, email string, executor interface {
	ExecContext(context.Context, string, ...any) (sql.Result, error)
}) (sessionResponse, error) {
	accessToken, expiresAt, err := api.tokens.AccessToken(userID, email)
	if err != nil {
		return sessionResponse{}, err
	}
	refreshToken, err := auth.RandomToken(48)
	if err != nil {
		return sessionResponse{}, err
	}
	refreshExpires := time.Now().UTC().Add(api.cfg.RefreshTTL)
	_, err = executor.ExecContext(r.Context(), `INSERT INTO refresh_tokens(token_hash,user_id,expires_at,created_at) VALUES(?,?,?,?)`, auth.TokenHash(refreshToken), userID, refreshExpires.Format(time.RFC3339Nano), time.Now().UTC().Format(time.RFC3339Nano))
	if err != nil {
		return sessionResponse{}, err
	}
	return sessionResponse{AccessToken: accessToken, RefreshToken: refreshToken, ExpiresAt: expiresAt, User: userResponse{ID: userID, Email: email}}, nil
}

func identifier() (string, error) {
	value := make([]byte, 16)
	if _, err := rand.Read(value); err != nil {
		return "", err
	}
	return hex.EncodeToString(value), nil
}
func containsUnique(err error) bool {
	return err != nil && strings.Contains(err.Error(), "UNIQUE constraint failed")
}

const (
	accessCookieName  = "balance_access"
	refreshCookieName = "balance_refresh"
)

func (api *API) setSessionCookies(w http.ResponseWriter, r *http.Request, session sessionResponse) {
	secure := requestIsSecure(r)
	http.SetCookie(w, &http.Cookie{
		Name: accessCookieName, Value: session.AccessToken, Path: "/", HttpOnly: true,
		Secure: secure, SameSite: http.SameSiteStrictMode,
		Expires: session.ExpiresAt, MaxAge: maxAge(session.ExpiresAt),
	})
	refreshExpires := time.Now().UTC().Add(api.cfg.RefreshTTL)
	http.SetCookie(w, &http.Cookie{
		Name: refreshCookieName, Value: session.RefreshToken, Path: "/", HttpOnly: true,
		Secure: secure, SameSite: http.SameSiteStrictMode,
		Expires: refreshExpires, MaxAge: maxAge(refreshExpires),
	})
}

func (api *API) clearSessionCookies(w http.ResponseWriter, r *http.Request) {
	for _, name := range []string{accessCookieName, refreshCookieName} {
		http.SetCookie(w, &http.Cookie{
			Name: name, Value: "", Path: "/", HttpOnly: true, Secure: requestIsSecure(r),
			SameSite: http.SameSiteStrictMode, MaxAge: -1, Expires: time.Unix(1, 0),
		})
	}
}

func (api *API) refreshToken(r *http.Request, value string) string {
	if value = strings.TrimSpace(value); value != "" {
		return value
	}
	if cookie, err := r.Cookie(refreshCookieName); err == nil {
		return strings.TrimSpace(cookie.Value)
	}
	return ""
}

func requestIsSecure(r *http.Request) bool {
	return r.TLS != nil || strings.EqualFold(strings.TrimSpace(r.Header.Get("X-Forwarded-Proto")), "https")
}

func maxAge(expires time.Time) int {
	seconds := int(time.Until(expires).Seconds())
	if seconds < 1 {
		return 1
	}
	return seconds
}
