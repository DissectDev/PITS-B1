package main

import (
	"crypto/rand"
	"crypto/tls"
	"encoding/hex"
	"flag"
	"log"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"pits.io/server/internal/pep"
)

func splitCSV(raw string) map[string]bool {
	out := make(map[string]bool)
	for _, part := range strings.Split(raw, ",") {
		p := strings.TrimSpace(part)
		if p != "" {
			out[p] = true
		}
	}
	return out
}

func splitList(raw string) []string {
	var out []string
	for _, part := range strings.Split(raw, ",") {
		p := strings.TrimSpace(part)
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

func main() {
	var (
		listenAddr       = flag.String("listen", "100.95.95.88:18444", "TLS listen address for Mode-A PEP gateway")
		upstreamRaw      = flag.String("upstream", "http://127.0.0.1:18090", "Upstream protected service URL")
		tailscaledSock   = flag.String("tailscaled-sock", "/var/run/tailscale/tailscaled.sock", "Path to tailscaled Unix socket")
		requireUserOwned = flag.Bool("require-user-owned", true, "Require Tailscale peers to be user-owned (reject tagged devices unless in -allowed-tags)")
		allowedTags      = flag.String("allowed-tags", "", "Comma-separated allowed Tailscale tags")
		allowedUsers     = flag.String("allowed-users", "", "Comma-separated allowed Tailscale user logins (empty = any tailnet user)")
		allowedOS        = flag.String("allowed-os", "windows,linux,macos", "Comma-separated allowed device OSes")
		requiredCap      = flag.String("required-cap", "", "Optional Tailscale Grant capability key required on peer")
		issuerURL        = flag.String("oidc-issuer", "https://sso.martensite.dev/realms/corp", "Keycloak realm issuer URL")
		externalBaseURL  = flag.String("external-base-url", "https://100.95.95.88:18444", "External base URL for OIDC redirect_uri")
		clientID         = flag.String("oidc-client-id", "pits-mode-a-gateway", "Keycloak OIDC Client ID")
		clientSecretFile = flag.String("oidc-client-secret-file", "", "Path to file containing OIDC client secret")
		caCertPath       = flag.String("oidc-ca-cert", "", "Path to CA certificate PEM (e.g. pits-ca) for Keycloak")
		dialOverride     = flag.String("oidc-dial-override", "127.0.0.1:443", "Optional host:port override when connecting to OIDC issuer")
		requiredACR      = flag.String("required-acr", "fido2", "Comma-separated accepted ACR values")
		requiredAMR      = flag.String("required-amr", "", "Optional comma-separated required AMR values")
		sessionTTL       = flag.Duration("session-ttl", 8*time.Hour, "Encrypted PEP session lifetime")
		sessionKeyFile   = flag.String("session-key-file", "", "Optional file containing 32-byte/hex session key")
		tlsCertFile      = flag.String("tls-cert", "", "Server TLS certificate PEM")
		tlsKeyFile       = flag.String("tls-key", "", "Server TLS private key PEM")
	)
	flag.Parse()

	upstreamURL, err := url.Parse(*upstreamRaw)
	if err != nil {
		log.Fatalf("invalid -upstream: %v", err)
	}

	var clientSecret string
	if *clientSecretFile != "" {
		b, err := os.ReadFile(*clientSecretFile)
		if err != nil {
			log.Fatalf("read -oidc-client-secret-file: %v", err)
		}
		clientSecret = strings.TrimSpace(string(b))
	} else {
		clientSecret = strings.TrimSpace(os.Getenv("OIDC_CLIENT_SECRET"))
	}

	var sessionKey []byte
	if *sessionKeyFile != "" {
		b, err := os.ReadFile(*sessionKeyFile)
		if err != nil {
			log.Fatalf("read -session-key-file: %v", err)
		}
		raw := strings.TrimSpace(string(b))
		if decoded, err := hex.DecodeString(raw); err == nil && len(decoded) == 32 {
			sessionKey = decoded
		} else {
			sessionKey = []byte(raw)
		}
	} else {
		sessionKey = make([]byte, 32)
		if _, err := rand.Read(sessionKey); err != nil {
			log.Fatalf("rand session key: %v", err)
		}
	}

	tsVerifier := pep.NewLocalAPIVerifier(*tailscaledSock, pep.TailscalePolicy{
		RequireUserOwned: *requireUserOwned,
		AllowedTags:      splitCSV(*allowedTags),
		AllowedUsers:     splitCSV(*allowedUsers),
		AllowedOS:        splitCSV(strings.ToLower(*allowedOS)),
		RequiredCap:      strings.TrimSpace(*requiredCap),
	})

	oidcVerifier, err := pep.NewOIDCVerifier(pep.OIDCConfig{
		IssuerURL:         *issuerURL,
		ExternalBaseURL:   *externalBaseURL,
		ClientID:          *clientID,
		ClientSecret:      clientSecret,
		CACertPath:        *caCertPath,
		DialOverrideAddr:  *dialOverride,
		RequiredACRValues: splitList(*requiredACR),
		RequiredAMRValues: splitList(*requiredAMR),
		SessionTTL:        *sessionTTL,
		SessionKey:        sessionKey,
	})
	if err != nil {
		log.Fatalf("init OIDC verifier: %v", err)
	}

	handler := pep.NewGatewayHandler(pep.GatewayConfig{
		UpstreamURL: upstreamURL,
		TS:          tsVerifier,
		OIDC:        oidcVerifier,
	})

	srv := &http.Server{
		Addr:              *listenAddr,
		Handler:           handler,
		ReadHeaderTimeout: 10 * time.Second,
	}

	log.Printf("pits-pep-gateway (Mode-A PEP) listening on %s -> %s (issuer=%s, requiredACR=%s)",
		*listenAddr, *upstreamRaw, *issuerURL, *requiredACR)

	if *tlsCertFile != "" && *tlsKeyFile != "" {
		srv.TLSConfig = &tls.Config{MinVersion: tls.VersionTLS12}
		log.Fatal(srv.ListenAndServeTLS(*tlsCertFile, *tlsKeyFile))
	} else {
		log.Fatal(srv.ListenAndServe())
	}
}
