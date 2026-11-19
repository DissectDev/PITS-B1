package pep

import (
	"context"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"
)

type ctxKey string

const (
	ctxKeyPeer  ctxKey = "pep-peer"
	ctxKeyHuman ctxKey = "pep-human"
)

// GatewayConfig configures the Mode A Policy Enforcement Point (PEP) reverse proxy.
type GatewayConfig struct {
	UpstreamURL *url.URL
	TS          TailscaleVerifier
	OIDC        *OIDCVerifier
}

// NewGatewayHandler builds the Mode A PEP HTTP handler enforcing Tailscale Whois + Keycloak FIDO2 OIDC.
func NewGatewayHandler(cfg GatewayConfig) http.Handler {
	proxy := httputil.NewSingleHostReverseProxy(cfg.UpstreamURL)
	origDirector := proxy.Director
	proxy.Director = func(req *http.Request) {
		origDirector(req)
		// Strip any client-spoofed identity headers before forwarding.
		for k := range req.Header {
			lk := strings.ToLower(k)
			if strings.HasPrefix(lk, "x-pits-") || strings.HasPrefix(lk, "x-auth-") {
				req.Header.Del(k)
			}
		}
		if peer, ok := req.Context().Value(ctxKeyPeer).(*PeerIdentity); ok && peer != nil {
			req.Header.Set("X-Pits-Tailscale-Node", peer.NodeName)
			req.Header.Set("X-Pits-Tailscale-ID", peer.NodeID)
			req.Header.Set("X-Pits-Tailscale-IP", peer.TailnetIP)
			req.Header.Set("X-Pits-Tailscale-User", peer.LoginName)
			req.Header.Set("X-Pits-Device-OS", peer.OS)
			if !peer.KeyExpiry.IsZero() {
				req.Header.Set("X-Pits-Device-Key-Expiry", peer.KeyExpiry.UTC().Format(time.RFC3339))
			}
		}
		if human, ok := req.Context().Value(ctxKeyHuman).(*HumanIdentity); ok && human != nil {
			req.Header.Set("X-Auth-User", human.Username)
			req.Header.Set("X-Auth-Email", human.Email)
			req.Header.Set("X-Auth-Subject", human.Subject)
			req.Header.Set("X-Auth-ACR", human.ACR)
			if len(human.AMR) > 0 {
				req.Header.Set("X-Auth-AMR", strings.Join(human.AMR, ","))
			}
		}
	}

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Local-only unauthenticated health check for systemd / local probes.
		if r.URL.Path == "/healthz" && isLoopbackRemote(r.RemoteAddr) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"status":"ok","mode":"pep-a"}`))
			return
		}

		// Step 4a: Mandatory Tailscale machine identity + device posture check.
		peer, err := cfg.TS.VerifyPeer(r.Context(), r.RemoteAddr)
		if err != nil {
			http.Error(w, "Mode-A PEP denied (Tailscale Whois/Posture): "+err.Error(), http.StatusForbidden)
			return
		}

		// Handle OIDC callback & logout on the gateway itself.
		if r.URL.Path == cfg.OIDC.cfg.RedirectPath {
			_, sessCookie, returnTo, err := cfg.OIDC.CompleteAuth(r.Context(), r, peer)
			if err != nil {
				http.Error(w, "Mode-A PEP OIDC callback failed: "+err.Error(), http.StatusForbidden)
				return
			}
			http.SetCookie(w, &http.Cookie{
				Name:     "pits_pep_state",
				Value:    "",
				Path:     "/",
				MaxAge:   -1,
				HttpOnly: true,
				Secure:   r.TLS != nil,
			})
			http.SetCookie(w, sessCookie)
			http.Redirect(w, r, returnTo, http.StatusFound)
			return
		}

		if r.URL.Path == "/oauth2/logout" {
			http.SetCookie(w, &http.Cookie{
				Name:     "pits_pep_session",
				Value:    "",
				Path:     "/",
				MaxAge:   -1,
				HttpOnly: true,
				Secure:   r.TLS != nil,
			})
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"loggedOut":true}`))
			return
		}

		// Step 4b: Human identity + FIDO2 strength (Bearer JWT or bound encrypted session cookie).
		var human *HumanIdentity
		if authz := r.Header.Get("Authorization"); strings.HasPrefix(strings.ToLower(authz), "bearer ") {
			raw := strings.TrimSpace(authz[7:])
			human, err = cfg.OIDC.VerifyJWT(r.Context(), raw, false, "")
			if err != nil {
				http.Error(w, "Mode-A PEP denied (Bearer JWT/ACR): "+err.Error(), http.StatusForbidden)
				return
			}
			human.NodeID = peer.NodeID
		} else {
			human, err = cfg.OIDC.VerifySession(r, peer)
			if err != nil {
				if wantsHTML(r) {
					authURL, stateCookie, startErr := cfg.OIDC.StartAuth(r.Context(), r, peer)
					if startErr != nil {
						http.Error(w, "Mode-A PEP OIDC init failed: "+startErr.Error(), http.StatusBadGateway)
						return
					}
					http.SetCookie(w, stateCookie)
					http.Redirect(w, r, authURL, http.StatusFound)
					return
				}
				http.Error(w, "Mode-A PEP unauthorized: "+err.Error(), http.StatusUnauthorized)
				return
			}
		}

		if r.URL.Path == "/oauth2/userinfo" {
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(map[string]any{
				"human":   human,
				"machine": peer,
			})
			return
		}

		ctx := context.WithValue(r.Context(), ctxKeyPeer, peer)
		ctx = context.WithValue(ctx, ctxKeyHuman, human)
		proxy.ServeHTTP(w, r.WithContext(ctx))
	})
}

func wantsHTML(r *http.Request) bool {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		return false
	}
	if strings.HasPrefix(r.URL.Path, "/api/") {
		return false
	}
	accept := r.Header.Get("Accept")
	return strings.Contains(accept, "text/html") || accept == "" || accept == "*/*"
}

func isLoopbackRemote(remoteAddr string) bool {
	host, _, err := net.SplitHostPort(remoteAddr)
	if err != nil {
		host = remoteAddr
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
