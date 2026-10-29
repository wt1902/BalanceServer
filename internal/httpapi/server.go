package httpapi

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"balance-server/internal/auth"
	"balance-server/internal/config"
)

type contextKey string

const claimsKey contextKey = "claims"

type API struct {
	cfg     config.Config
	db      *sql.DB
	tokens  auth.TokenManager
	logger  *slog.Logger
	limiter *rateLimiter
}

func New(cfg config.Config, db *sql.DB, logger *slog.Logger) http.Handler {
	api := &API{cfg: cfg, db: db, tokens: auth.NewTokenManager(cfg.JWTSecret, cfg.AccessTTL), logger: logger, limiter: newRateLimiter()}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", api.health)
	mux.Handle("POST /v1/auth/register", api.limited(http.HandlerFunc(api.register)))
	mux.Handle("POST /v1/auth/login", api.limited(http.HandlerFunc(api.login)))
	mux.Handle("POST /v1/auth/refresh", api.limited(http.HandlerFunc(api.refresh)))
	mux.HandleFunc("POST /v1/auth/logout", api.logout)
	mux.Handle("GET /v1/me", api.authorized(http.HandlerFunc(api.me)))
	mux.Handle("POST /v1/sync", api.authorized(http.HandlerFunc(api.sync)))
	mux.HandleFunc("GET /", api.webApp)
	return api.recover(api.security(api.cors(mux)))
}

func (api *API) health(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"status": "ok", "time": time.Now().UTC()})
}

func (api *API) authorized(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		header := strings.TrimSpace(r.Header.Get("Authorization"))
		token := ""
		if strings.HasPrefix(header, "Bearer ") {
			token = strings.TrimSpace(strings.TrimPrefix(header, "Bearer "))
		} else if cookie, err := r.Cookie(accessCookieName); err == nil {
			token = strings.TrimSpace(cookie.Value)
		}
		if token == "" {
			writeError(w, http.StatusUnauthorized, "authorization_required", "Authorization Bearer token is required")
			return
		}
		claims, err := api.tokens.Verify(token)
		if err != nil {
			writeError(w, http.StatusUnauthorized, "invalid_token", "Access token is invalid or expired")
			return
		}
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), claimsKey, claims)))
	})
}

func claims(r *http.Request) auth.Claims {
	value, _ := r.Context().Value(claimsKey).(auth.Claims)
	return value
}

func (api *API) security(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.Header().Set("X-Frame-Options", "DENY")
		w.Header().Set("Referrer-Policy", "no-referrer")
		w.Header().Set("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=()")
		w.Header().Set("Content-Security-Policy", "default-src 'self'; base-uri 'none'; connect-src 'self'; font-src 'self'; form-action 'self'; frame-ancestors 'none'; img-src 'self' data:; manifest-src 'self'; script-src 'self'; style-src 'self'; worker-src 'self'")
		if strings.HasPrefix(r.URL.Path, "/v1/") {
			w.Header().Set("Cache-Control", "no-store")
		}
		next.ServeHTTP(w, r)
	})
}

func (api *API) cors(next http.Handler) http.Handler {
	allowed := make(map[string]bool, len(api.cfg.CORSOrigins))
	for _, origin := range api.cfg.CORSOrigins {
		allowed[origin] = true
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		origin := r.Header.Get("Origin")
		if origin != "" && (allowed[origin] || allowed["*"]) {
			w.Header().Set("Access-Control-Allow-Origin", origin)
			w.Header().Set("Vary", "Origin")
			w.Header().Set("Access-Control-Allow-Headers", "Authorization, Content-Type")
			w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		}
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func (api *API) recover(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() {
			if value := recover(); value != nil {
				api.logger.Error("request panic", "value", value)
				writeError(w, http.StatusInternalServerError, "internal_error", "Internal server error")
			}
		}()
		next.ServeHTTP(w, r)
	})
}

func decodeJSON(w http.ResponseWriter, r *http.Request, destination any) bool {
	r.Body = http.MaxBytesReader(w, r.Body, 2<<20)
	decoder := json.NewDecoder(r.Body)
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil {
		writeError(w, http.StatusBadRequest, "invalid_json", "Request body is not valid JSON")
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func writeError(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]any{"error": map[string]string{"code": code, "message": message}})
}

type rateLimiter struct {
	mu      sync.Mutex
	clients map[string]*rateBucket
}
type rateBucket struct {
	started time.Time
	count   int
}

func newRateLimiter() *rateLimiter { return &rateLimiter{clients: make(map[string]*rateBucket)} }
func (api *API) limited(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		host, _, err := net.SplitHostPort(r.RemoteAddr)
		if err != nil {
			host = r.RemoteAddr
		}
		now := time.Now()
		api.limiter.mu.Lock()
		bucket := api.limiter.clients[host]
		if bucket == nil || now.Sub(bucket.started) >= time.Minute {
			bucket = &rateBucket{started: now}
			api.limiter.clients[host] = bucket
		}
		bucket.count++
		blocked := bucket.count > 30
		api.limiter.mu.Unlock()
		if blocked {
			w.Header().Set("Retry-After", "60")
			writeError(w, http.StatusTooManyRequests, "rate_limited", "Too many authentication requests")
			return
		}
		next.ServeHTTP(w, r)
	})
}

func normalizeEmail(value string) (string, error) {
	email := strings.ToLower(strings.TrimSpace(value))
	if len(email) < 3 || len(email) > 254 || !strings.Contains(email, "@") {
		return "", errors.New("invalid email")
	}
	return email, nil
}
