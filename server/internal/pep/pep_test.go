package pep

import (
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

func signTestJWT(t *testing.T, priv *rsa.PrivateKey, kid string, claims map[string]any) string {
	t.Helper()
	hdrJSON, _ := json.Marshal(map[string]string{"alg": "RS256", "typ": "JWT", "kid": kid})
	claimsJSON, _ := json.Marshal(claims)
	h := base64.RawURLEncoding.EncodeToString(hdrJSON)
	p := base64.RawURLEncoding.EncodeToString(claimsJSON)
	input := h + "." + p
	sum := sha256.Sum256([]byte(input))
	sig, err := rsa.SignPKCS1v15(rand.Reader, priv, crypto.SHA256, sum[:])
	if err != nil {
		t.Fatalf("sign jwt: %v", err)
	}
	return input + "." + base64.RawURLEncoding.EncodeToString(sig)
}

func TestModeAGatewayJointEnforcement(t *testing.T) {
	priv, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatalf("rsa key: %v", err)
	}
	const kid = "test-kid-1"

	// Mock OIDC IdP (Keycloak)
	var idpURL string
	idpSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/realms/corp/.well-known/openid-configuration":
			_ = json.NewEncoder(w).Encode(map[string]string{
				"issuer":                 idpURL + "/realms/corp",
				"authorization_endpoint": idpURL + "/realms/corp/protocol/openid-connect/auth",
				"token_endpoint":         idpURL + "/realms/corp/protocol/openid-connect/token",
				"jwks_uri":               idpURL + "/realms/corp/protocol/openid-connect/certs",
			})
		case "/realms/corp/protocol/openid-connect/certs":
			_ = json.NewEncoder(w).Encode(map[string]any{
				"keys": []map[string]string{{
					"kty": "RSA",
					"kid": kid,
					"use": "sig",
					"alg": "RS256",
					"n":   base64.RawURLEncoding.EncodeToString(priv.N.Bytes()),
					"e":   base64.RawURLEncoding.EncodeToString(big.NewInt(int64(priv.E)).Bytes()),
				}},
			})
		default:
			http.NotFound(w, r)
		}
	}))
	defer idpSrv.Close()
	idpURL = idpSrv.URL

	// Mock Tailscale LocalAPI Whois server
	whoisMux := http.NewServeMux()
	whoisMux.HandleFunc("/localapi/v0/whois", func(w http.ResponseWriter, r *http.Request) {
		addr := r.URL.Query().Get("addr")
		switch {
		case strings.HasPrefix(addr, "100.64.0.10:"):
			_ = json.NewEncoder(w).Encode(map[string]any{
				"Node": map[string]any{
					"ID":        10,
					"StableID":  "node-user-10",
					"Name":      "alice-laptop.tailnet.ts.net.",
					"KeyExpiry": time.Now().Add(12 * time.Hour).Format(time.RFC3339),
					"Hostinfo":  map[string]string{"OS": "windows"},
				},
				"UserProfile": map[string]any{"ID": 1, "LoginName": "alice@corp.example.com"},
			})
		case strings.HasPrefix(addr, "100.64.0.20:"):
			_ = json.NewEncoder(w).Encode(map[string]any{
				"Node": map[string]any{
					"ID":        20,
					"StableID":  "node-tagged-20",
					"Name":      "ci-runner.tailnet.ts.net.",
					"Tags":      []string{"tag:ci"},
					"KeyExpiry": time.Now().Add(12 * time.Hour).Format(time.RFC3339),
					"Hostinfo":  map[string]string{"OS": "linux"},
				},
				"UserProfile": map[string]any{"ID": 2, "LoginName": "tagged-devices"},
			})
		case strings.HasPrefix(addr, "100.64.0.30:"):
			_ = json.NewEncoder(w).Encode(map[string]any{
				"Node": map[string]any{
					"ID":        30,
					"StableID":  "node-expired-30",
					"Name":      "stale-laptop.tailnet.ts.net.",
					"KeyExpiry": time.Now().Add(-1 * time.Hour).Format(time.RFC3339),
					"Hostinfo":  map[string]string{"OS": "linux"},
				},
				"UserProfile": map[string]any{"ID": 3, "LoginName": "bob@corp.example.com"},
			})
		default:
			http.Error(w, "not found", http.StatusNotFound)
		}
	})
	whoisSrv := httptest.NewServer(whoisMux)
	defer whoisSrv.Close()

	var capturedHeaders http.Header
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		capturedHeaders = r.Header.Clone()
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("upstream-ok"))
	}))
	defer upstream.Close()
	upURL, _ := url.Parse(upstream.URL)

	tsVerifier := NewHTTPWhoisVerifier(whoisSrv.URL, whoisSrv.Client(), TailscalePolicy{
		RequireUserOwned: true,
		AllowedOS:        map[string]bool{"windows": true, "linux": true},
	})
	oidcVerifier, err := NewOIDCVerifier(OIDCConfig{
		IssuerURL:         idpURL + "/realms/corp",
		ClientID:          "pits-mode-a-gateway",
		RequiredACRValues: []string{"fido2"},
	})
	if err != nil {
		t.Fatalf("NewOIDCVerifier: %v", err)
	}

	gw := NewGatewayHandler(GatewayConfig{
		UpstreamURL: upURL,
		TS:          tsVerifier,
		OIDC:        oidcVerifier,
	})

	fido2Token := signTestJWT(t, priv, kid, map[string]any{
		"iss":                idpURL + "/realms/corp",
		"sub":                "user-uuid-1",
		"aud":                "pits-mode-a-gateway",
		"exp":                time.Now().Add(10 * time.Minute).Unix(),
		"acr":                "fido2",
		"amr":                []string{"pwd", "hwk"},
		"preferred_username": "alice",
		"email":              "alice@corp.example.com",
	})
	weakToken := signTestJWT(t, priv, kid, map[string]any{
		"iss":                idpURL + "/realms/corp",
		"sub":                "user-uuid-1",
		"aud":                "pits-mode-a-gateway",
		"exp":                time.Now().Add(10 * time.Minute).Unix(),
		"acr":                "1",
		"preferred_username": "alice",
	})

	// 1. Non-Tailscale IP -> 403
	req := httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/", nil)
	req.RemoteAddr = "192.168.1.50:54321"
	req.Header.Set("Authorization", "Bearer "+fido2Token)
	rec := httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("non-tailscale IP: got %d want 403", rec.Code)
	}

	// 2. Tagged device when user-owned required -> 403
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/", nil)
	req.RemoteAddr = "100.64.0.20:54321"
	req.Header.Set("Authorization", "Bearer "+fido2Token)
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("tagged device: got %d want 403", rec.Code)
	}

	// 3. Expired node key -> 403
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/", nil)
	req.RemoteAddr = "100.64.0.30:54321"
	req.Header.Set("Authorization", "Bearer "+fido2Token)
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("expired node key: got %d want 403", rec.Code)
	}

	// 4. Valid machine + weak ACR (acr=1 instead of fido2) -> 403
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/", nil)
	req.RemoteAddr = "100.64.0.10:54321"
	req.Header.Set("Authorization", "Bearer "+weakToken)
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusForbidden {
		t.Fatalf("weak ACR token: got %d want 403", rec.Code)
	}

	// 5. Valid machine + valid FIDO2 token -> 200 & spoofed headers stripped
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/api/pits/v1/health", nil)
	req.RemoteAddr = "100.64.0.10:54321"
	req.Header.Set("Authorization", "Bearer "+fido2Token)
	req.Header.Set("X-Auth-User", "spoofed-admin")
	req.Header.Set("X-Pits-Tailscale-User", "spoofed-peer")
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("valid joint check: got %d want 200 (%s)", rec.Code, rec.Body.String())
	}
	if got := capturedHeaders.Get("X-Auth-User"); got != "alice" {
		t.Fatalf("X-Auth-User = %q want alice", got)
	}
	if got := capturedHeaders.Get("X-Auth-ACR"); got != "fido2" {
		t.Fatalf("X-Auth-ACR = %q want fido2", got)
	}
	if got := capturedHeaders.Get("X-Pits-Tailscale-User"); got != "alice@corp.example.com" {
		t.Fatalf("X-Pits-Tailscale-User = %q want alice@corp.example.com", got)
	}

	// 6. Browser request without session -> 302 redirect to Keycloak with acr_values=fido2
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/", nil)
	req.RemoteAddr = "100.64.0.10:54321"
	req.Header.Set("Accept", "text/html")
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusFound {
		t.Fatalf("browser unauth: got %d want 302", rec.Code)
	}
	loc := rec.Header().Get("Location")
	if !strings.Contains(loc, "acr_values=fido2") || !strings.Contains(loc, "code_challenge_method=S256") {
		t.Fatalf("unexpected OIDC redirect URL: %s", loc)
	}

	// 7. Bound session cookie replay from DIFFERENT Tailscale node -> 401
	sessVal, err := oidcVerifier.seal(&HumanIdentity{
		Subject:  "user-uuid-1",
		Username: "alice",
		ACR:      "fido2",
		NodeID:   "node-other-999",
		Exp:      time.Now().Add(1 * time.Hour).Unix(),
	})
	if err != nil {
		t.Fatalf("seal session: %v", err)
	}
	req = httptest.NewRequest(http.MethodGet, "https://portal.pits.home.arpa/api/catalog/entities", nil)
	req.RemoteAddr = "100.64.0.10:54321"
	req.AddCookie(&http.Cookie{Name: "pits_pep_session", Value: sessVal})
	rec = httptest.NewRecorder()
	gw.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("replayed cookie from wrong node: got %d want 401", rec.Code)
	}
}
