package auth

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"strings"
	"time"
)

type Claims struct {
	Subject string `json:"sub"`
	Email   string `json:"email"`
	Issuer  string `json:"iss"`
	Issued  int64  `json:"iat"`
	Expires int64  `json:"exp"`
	ID      string `json:"jti"`
}

type TokenManager struct {
	secret    []byte
	accessTTL time.Duration
}

func NewTokenManager(secret string, accessTTL time.Duration) TokenManager {
	return TokenManager{secret: []byte(secret), accessTTL: accessTTL}
}

func (manager TokenManager) AccessToken(userID, email string) (string, time.Time, error) {
	now := time.Now().UTC()
	expires := now.Add(manager.accessTTL)
	id, err := RandomToken(16)
	if err != nil {
		return "", time.Time{}, err
	}
	header, _ := json.Marshal(map[string]string{"alg": "HS256", "typ": "JWT"})
	claims, _ := json.Marshal(Claims{Subject: userID, Email: email, Issuer: "balance-server", Issued: now.Unix(), Expires: expires.Unix(), ID: id})
	unsigned := encode(header) + "." + encode(claims)
	signature := manager.sign(unsigned)
	return unsigned + "." + signature, expires, nil
}

func (manager TokenManager) Verify(token string) (Claims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return Claims{}, errors.New("invalid token")
	}
	unsigned := parts[0] + "." + parts[1]
	expected := manager.sign(unsigned)
	if !hmac.Equal([]byte(parts[2]), []byte(expected)) {
		return Claims{}, errors.New("invalid token signature")
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return Claims{}, errors.New("invalid token payload")
	}
	var claims Claims
	if err := json.Unmarshal(payload, &claims); err != nil {
		return Claims{}, errors.New("invalid token claims")
	}
	if claims.Issuer != "balance-server" || claims.Subject == "" || time.Now().UTC().Unix() >= claims.Expires {
		return Claims{}, errors.New("expired or invalid token")
	}
	return claims, nil
}

func RandomToken(bytes int) (string, error) {
	value := make([]byte, bytes)
	if _, err := rand.Read(value); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(value), nil
}

func TokenHash(token string) string {
	hash := sha256.Sum256([]byte(token))
	return hex.EncodeToString(hash[:])
}

func (manager TokenManager) sign(value string) string {
	mac := hmac.New(sha256.New, manager.secret)
	_, _ = mac.Write([]byte(value))
	return base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
}

func encode(value []byte) string { return base64.RawURLEncoding.EncodeToString(value) }
