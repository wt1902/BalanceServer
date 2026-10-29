package httpapi

import (
	"database/sql"
	"encoding/json"
	"errors"
	"net/http"
	"time"
)

const maxSyncChanges = 5000
const syncPageSize = 500

type syncChange struct {
	Entity    string          `json:"entity"`
	ID        string          `json:"id"`
	Deleted   bool            `json:"deleted"`
	UpdatedAt time.Time       `json:"updatedAt"`
	Payload   json.RawMessage `json:"payload,omitempty"`
	Version   int64           `json:"version,omitempty"`
	Sequence  int64           `json:"sequence,omitempty"`
}
type syncRequest struct {
	Cursor   int64        `json:"cursor"`
	DeviceID string       `json:"deviceId"`
	Changes  []syncChange `json:"changes"`
	Pull     *bool        `json:"pull,omitempty"`
}
type syncResponse struct {
	Cursor     int64        `json:"cursor"`
	Changes    []syncChange `json:"changes"`
	ServerTime time.Time    `json:"serverTime"`
	HasMore    bool         `json:"hasMore"`
}

func (api *API) sync(w http.ResponseWriter, r *http.Request) {
	var request syncRequest
	if !decodeJSON(w, r, &request) {
		return
	}
	if request.Cursor < 0 || len(request.Changes) > maxSyncChanges {
		writeError(w, http.StatusBadRequest, "invalid_sync_request", "Cursor or change count is invalid")
		return
	}
	for index := range request.Changes {
		if err := validateChange(request.Changes[index]); err != nil {
			writeError(w, http.StatusBadRequest, "invalid_change", err.Error())
			return
		}
	}
	userID := claims(r).Subject
	tx, err := api.db.BeginTx(r.Context(), nil)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "sync_failed", "Could not begin synchronization")
		return
	}
	defer tx.Rollback()
	for _, change := range request.Changes {
		if err := applyChange(r, tx, userID, change); err != nil {
			api.logger.Error("apply sync change", "error", err)
			writeError(w, http.StatusInternalServerError, "sync_failed", "Could not apply changes")
			return
		}
	}
	changes := make([]syncChange, 0)
	cursor := request.Cursor
	hasMore := false
	shouldPull := request.Pull == nil || *request.Pull
	if shouldPull {
		changes, cursor, hasMore, err = loadChanges(r, tx, userID, request.Cursor)
		if err != nil {
			api.logger.Error("load sync changes", "error", err)
			writeError(w, http.StatusInternalServerError, "sync_failed", "Could not load changes")
			return
		}
	}
	if err := tx.Commit(); err != nil {
		writeError(w, http.StatusInternalServerError, "sync_failed", "Could not complete synchronization")
		return
	}
	writeJSON(w, http.StatusOK, syncResponse{Cursor: cursor, Changes: changes, ServerTime: time.Now().UTC(), HasMore: hasMore})
}

func validateChange(change syncChange) error {
	if change.Entity != "transaction" && change.Entity != "category" && change.Entity != "budget" {
		return errors.New("entity must be transaction, category, or budget")
	}
	if len(change.ID) < 1 || len(change.ID) > 128 {
		return errors.New("record id is invalid")
	}
	if change.UpdatedAt.IsZero() {
		return errors.New("updatedAt is required")
	}
	if change.UpdatedAt.After(time.Now().UTC().Add(24 * time.Hour)) {
		return errors.New("updatedAt is too far in the future")
	}
	if !change.Deleted && (len(change.Payload) == 0 || !json.Valid(change.Payload)) {
		return errors.New("payload is required for active records")
	}
	if len(change.Payload) > 256*1024 {
		return errors.New("payload is too large")
	}
	return nil
}

func applyChange(r *http.Request, tx *sql.Tx, userID string, incoming syncChange) error {
	var currentUpdated string
	var currentVersion int64
	err := tx.QueryRowContext(r.Context(), `SELECT updated_at,version FROM records WHERE user_id=? AND entity=? AND record_id=?`, userID, incoming.Entity, incoming.ID).Scan(&currentUpdated, &currentVersion)
	if err != nil && !errors.Is(err, sql.ErrNoRows) {
		return err
	}
	if err == nil {
		parsed, parseErr := time.Parse(time.RFC3339Nano, currentUpdated)
		if parseErr != nil {
			return parseErr
		}
		if !incoming.UpdatedAt.After(parsed) {
			return nil
		}
	}
	version := currentVersion + 1
	updated := incoming.UpdatedAt.UTC().Format(time.RFC3339Nano)
	var payload any
	if !incoming.Deleted {
		payload = string(incoming.Payload)
	}
	_, err = tx.ExecContext(r.Context(), `INSERT INTO records(user_id,entity,record_id,payload,updated_at,deleted,version) VALUES(?,?,?,?,?,?,?)
		ON CONFLICT(user_id,entity,record_id) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at,deleted=excluded.deleted,version=excluded.version`,
		userID, incoming.Entity, incoming.ID, payload, updated, incoming.Deleted, version)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(r.Context(), `INSERT INTO changes(user_id,entity,record_id,payload,updated_at,deleted,version) VALUES(?,?,?,?,?,?,?)`, userID, incoming.Entity, incoming.ID, payload, updated, incoming.Deleted, version)
	return err
}

func loadChanges(r *http.Request, tx *sql.Tx, userID string, after int64) ([]syncChange, int64, bool, error) {
	rows, err := tx.QueryContext(r.Context(), `SELECT sequence,entity,record_id,payload,updated_at,deleted,version FROM changes WHERE user_id=? AND sequence>? ORDER BY sequence ASC LIMIT ?`, userID, after, syncPageSize+1)
	if err != nil {
		return nil, after, false, err
	}
	defer rows.Close()
	result := make([]syncChange, 0)
	cursor := after
	hasMore := false
	for rows.Next() {
		if len(result) == syncPageSize {
			hasMore = true
			break
		}
		var item syncChange
		var payload sql.NullString
		var updated string
		if err := rows.Scan(&item.Sequence, &item.Entity, &item.ID, &payload, &updated, &item.Deleted, &item.Version); err != nil {
			return nil, after, false, err
		}
		item.UpdatedAt, err = time.Parse(time.RFC3339Nano, updated)
		if err != nil {
			return nil, after, false, err
		}
		if payload.Valid {
			item.Payload = json.RawMessage(payload.String)
		}
		cursor = item.Sequence
		result = append(result, item)
	}
	return result, cursor, hasMore, rows.Err()
}
