package api

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHandlerHealthAndAuth(t *testing.T) {
	h := NewHandler(&Handler{
		Version:     "1.0.0-test",
		BearerToken: "secret-pits-token",
	})

	// Unauthenticated request must return 401.
	req1 := httptest.NewRequest(http.MethodGet, "http://pits-core/v1/health", nil)
	rec1 := httptest.NewRecorder()
	h.ServeHTTP(rec1, req1)
	if rec1.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without token, got %d", rec1.Code)
	}

	// Authenticated request returns ServerHealth JSON.
	req2 := httptest.NewRequest(http.MethodGet, "http://pits-core/v1/health", nil)
	req2.Header.Set("Authorization", "Bearer secret-pits-token")
	rec2 := httptest.NewRecorder()
	h.ServeHTTP(rec2, req2)
	if rec2.Code != http.StatusOK {
		t.Fatalf("expected 200 with valid token, got %d", rec2.Code)
	}
	var health ServerHealth
	if err := json.NewDecoder(rec2.Body).Decode(&health); err != nil {
		t.Fatal(err)
	}
	if health.Version != "1.0.0-test" || health.Protocol != ProtocolVersion {
		t.Fatalf("unexpected health response: %+v", health)
	}
}
