package config

import (
	"errors"
	"os"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	Address           string
	DatabasePath      string
	JWTSecret         string
	AccessTTL         time.Duration
	RefreshTTL        time.Duration
	AllowRegistration bool
	CORSOrigins       []string
}

func Load() (Config, error) {
	accessTTL, err := duration("BALANCE_ACCESS_TTL", 15*time.Minute)
	if err != nil {
		return Config{}, err
	}
	refreshTTL, err := duration("BALANCE_REFRESH_TTL", 30*24*time.Hour)
	if err != nil {
		return Config{}, err
	}
	allowRegistration, err := boolean("BALANCE_ALLOW_REGISTRATION", true)
	if err != nil {
		return Config{}, err
	}

	cfg := Config{
		Address:           env("BALANCE_ADDR", ":8080"),
		DatabasePath:      env("BALANCE_DB_PATH", "./data/balance.db"),
		JWTSecret:         strings.TrimSpace(os.Getenv("BALANCE_JWT_SECRET")),
		AccessTTL:         accessTTL,
		RefreshTTL:        refreshTTL,
		AllowRegistration: allowRegistration,
		CORSOrigins:       split(os.Getenv("BALANCE_CORS_ORIGINS")),
	}
	if len(cfg.JWTSecret) < 32 {
		return Config{}, errors.New("BALANCE_JWT_SECRET must contain at least 32 characters")
	}
	return cfg, nil
}

func env(key, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}

func duration(key string, fallback time.Duration) (time.Duration, error) {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback, nil
	}
	parsed, err := time.ParseDuration(value)
	if err != nil {
		return 0, errors.New(key + " must be a Go duration such as 15m or 720h")
	}
	return parsed, nil
}

func boolean(key string, fallback bool) (bool, error) {
	value := strings.TrimSpace(os.Getenv(key))
	if value == "" {
		return fallback, nil
	}
	parsed, err := strconv.ParseBool(value)
	if err != nil {
		return false, errors.New(key + " must be true or false")
	}
	return parsed, nil
}

func split(value string) []string {
	if strings.TrimSpace(value) == "" {
		return nil
	}
	parts := strings.Split(value, ",")
	result := make([]string, 0, len(parts))
	for _, part := range parts {
		if item := strings.TrimSpace(part); item != "" {
			result = append(result, item)
		}
	}
	return result
}
