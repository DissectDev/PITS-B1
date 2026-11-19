package pep

import (
	"context"
	"crypto"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"
)

// HumanIdentity represents the validated Keycloak OIDC user and FIDO2 authentication strength.
type HumanIdentity struct {
	Subject  string   `json:"sub"`
	Username string   `json:"username"`
	Email    string   `json:"email"`
	ACR      string   `json:"acr"`
	AMR      []string `json:"amr,omitempty"`
	NodeID   string   `json:"node_id"`
	Exp      int64    `json:"exp"`
}

// OIDCConfig configures Keycloak OIDC authorization code + PKCE and Bearer JWT verification.
type OIDCConfig struct {
	IssuerURL         string
	ExternalBaseURL   string // e.g. https://portal.pits.home.arpa or https://100.95.95.88:18444
	ClientID          string
	ClientSecret      string
	RedirectPath      string
	CACertPath        string
	DialOverrideAddr  string // optional host:port override (e.g. 127.0.0.1:443) for IssuerURL host
	RequiredACRValues []string
	RequiredAMRValues []string
	SessionTTL        time.Duration
	SessionKey        []byte // 32 bytes for AES-256-GCM; random ephemeral if empty
}

type oidcDiscovery struct {
	Issuer                string `json:"issuer"`
	AuthorizationEndpoint string `json:"authorization_endpoint"`
	TokenEndpoint         string `json:"token_endpoint"`
	JWKSUri               string `json:"jwks_uri"`
	EndSessionEndpoint    string `json:"end_session_endpoint"`
}

type jwkKey struct {
	Kty string `json:"kty"`
	Kid string `json:"kid"`
	Use string `json:"use"`
	Alg string `json:"alg"`
	N   string `json:"n"`
	E   string `json:"e"`
	Crv string `json:"crv"`
	X   string `json:"x"`
	Y   string `json:"y"`
}

type jwksDoc struct {
	Keys []jwkKey `json:"keys"`
}

type pkceState struct {
	State        string `json:"s"`
	Nonce        string `json:"n"`
	CodeVerifier string `json:"v"`
	ReturnTo     string `json:"r"`
	NodeID       string `json:"d"`
	Exp          int64  `json:"e"`
}

// OIDCVerifier validates Bearer JWTs and browser OIDC sessions against Keycloak.
type OIDCVerifier struct {
	cfg        OIDCConfig
	httpClient *http.Client
	aead       cipher.AEAD
	now        func() time.Time

	mu        sync.RWMutex
	discovery *oidcDiscovery
	keys      map[string]crypto.PublicKey
	keysAt    time.Time
}

// NewOIDCVerifier creates an OIDCVerifier with custom CA trust and optional dial override.
func NewOIDCVerifier(cfg OIDCConfig) (*OIDCVerifier, error) {
	if strings.TrimSpace(cfg.IssuerURL) == "" {
		return nil, errors.New("IssuerURL is required")
	}
	if strings.TrimSpace(cfg.ClientID) == "" {
		return nil, errors.New("ClientID is required")
	}
	if cfg.RedirectPath == "" {
		cfg.RedirectPath = "/oauth2/callback"
	}
	if cfg.SessionTTL <= 0 {
		cfg.SessionTTL = 8 * time.Hour
	}
	if len(cfg.RequiredACRValues) == 0 {
		cfg.RequiredACRValues = []string{"fido2"}
	}

	key := cfg.SessionKey
	if len(key) == 0 {
		key = make([]byte, 32)
		if _, err := rand.Read(key); err != nil {
			return nil, fmt.Errorf("generate session key: %w", err)
		}
	} else if len(key) != 32 {
		sum := sha256.Sum256(key)
		key = sum[:]
	}

	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}

	tlsCfg := &tls.Config{MinVersion: tls.VersionTLS12}
	if strings.TrimSpace(cfg.CACertPath) != "" {
		pemData, err := os.ReadFile(cfg.CACertPath)
		if err != nil {
			return nil, fmt.Errorf("read CA cert %q: %w", cfg.CACertPath, err)
		}
		pool, err := x509.SystemCertPool()
		if err != nil || pool == nil {
			pool = x509.NewCertPool()
		}
		if !pool.AppendCertsFromPEM(pemData) {
			return nil, fmt.Errorf("failed to parse CA cert PEM in %q", cfg.CACertPath)
		}
		tlsCfg.RootCAs = pool
	}

	issuerParsed, err := url.Parse(cfg.IssuerURL)
	if err != nil {
		return nil, fmt.Errorf("invalid IssuerURL: %w", err)
	}

	tr := &http.Transport{
		TLSClientConfig: tlsCfg,
		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
			var d net.Dialer
			if cfg.DialOverrideAddr != "" {
				host, _, _ := net.SplitHostPort(addr)
				if strings.EqualFold(host, issuerParsed.Hostname()) {
					return d.DialContext(ctx, network, cfg.DialOverrideAddr)
				}
			}
			return d.DialContext(ctx, network, addr)
		},
	}

	return &OIDCVerifier{
		cfg:        cfg,
		httpClient: &http.Client{Transport: tr, Timeout: 10 * time.Second},
		aead:       aead,
		now:        time.Now,
		keys:       make(map[string]crypto.PublicKey),
	}, nil
}

func (o *OIDCVerifier) getDiscovery(ctx context.Context) (*oidcDiscovery, error) {
	o.mu.RLock()
	d := o.discovery
	o.mu.RUnlock()
	if d != nil {
		return d, nil
	}

	discURL := strings.TrimRight(o.cfg.IssuerURL, "/") + "/.well-known/openid-configuration"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, discURL, nil)
	if err != nil {
		return nil, err
	}
	resp, err := o.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("oidc discovery failed: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("oidc discovery HTTP %d", resp.StatusCode)
	}
	var loaded oidcDiscovery
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&loaded); err != nil {
		return nil, fmt.Errorf("decode oidc discovery: %w", err)
	}

	o.mu.Lock()
	o.discovery = &loaded
	o.mu.Unlock()
	return &loaded, nil
}

func (o *OIDCVerifier) getKey(ctx context.Context, kid string, forceRefresh bool) (crypto.PublicKey, error) {
	o.mu.RLock()
	k, ok := o.keys[kid]
	fresh := time.Since(o.keysAt) < 10*time.Minute
	o.mu.RUnlock()
	if ok && !forceRefresh && fresh {
		return k, nil
	}

	disc, err := o.getDiscovery(ctx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, disc.JWKSUri, nil)
	if err != nil {
		return nil, err
	}
	resp, err := o.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("fetch jwks: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("jwks HTTP %d", resp.StatusCode)
	}
	var doc jwksDoc
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&doc); err != nil {
		return nil, fmt.Errorf("decode jwks: %w", err)
	}

	parsed := make(map[string]crypto.PublicKey, len(doc.Keys))
	for _, jk := range doc.Keys {
		pub, err := parseJWK(jk)
		if err == nil && jk.Kid != "" {
			parsed[jk.Kid] = pub
		}
	}

	o.mu.Lock()
	o.keys = parsed
	o.keysAt = o.now()
	o.mu.Unlock()

	k, ok = parsed[kid]
	if !ok {
		return nil, fmt.Errorf("signing key kid %q not found in JWKS", kid)
	}
	return k, nil
}

func parseJWK(jk jwkKey) (crypto.PublicKey, error) {
	switch jk.Kty {
	case "RSA":
		nb, err := base64.RawURLEncoding.DecodeString(jk.N)
		if err != nil {
			return nil, err
		}
		eb, err := base64.RawURLEncoding.DecodeString(jk.E)
		if err != nil {
			return nil, err
		}
		e := 0
		for _, b := range eb {
			e = (e << 8) | int(b)
		}
		if e <= 0 {
			return nil, errors.New("invalid RSA exponent")
		}
		return &rsa.PublicKey{N: new(big.Int).SetBytes(nb), E: e}, nil
	case "EC":
		xb, err := base64.RawURLEncoding.DecodeString(jk.X)
		if err != nil {
			return nil, err
		}
		yb, err := base64.RawURLEncoding.DecodeString(jk.Y)
		if err != nil {
			return nil, err
		}
		var curve elliptic.Curve
		switch jk.Crv {
		case "P-256":
			curve = elliptic.P256()
		case "P-384":
			curve = elliptic.P384()
		default:
			return nil, fmt.Errorf("unsupported EC curve %q", jk.Crv)
		}
		return &ecdsa.PublicKey{
			Curve: curve,
			X:     new(big.Int).SetBytes(xb),
			Y:     new(big.Int).SetBytes(yb),
		}, nil
	default:
		return nil, fmt.Errorf("unsupported JWK kty %q", jk.Kty)
	}
}

// VerifyJWT validates signature, issuer, expiration, audience, and FIDO2 ACR/AMR claims.
func (o *OIDCVerifier) VerifyJWT(ctx context.Context, rawToken string, requireAudience bool, expectedNonce string) (*HumanIdentity, error) {
	parts := strings.Split(rawToken, ".")
	if len(parts) != 3 {
		return nil, errors.New("invalid JWT format")
	}

	hdrBytes, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return nil, fmt.Errorf("decode jwt header: %w", err)
	}
	var hdr struct {
		Alg string `json:"alg"`
		Kid string `json:"kid"`
	}
	if err := json.Unmarshal(hdrBytes, &hdr); err != nil {
		return nil, fmt.Errorf("unmarshal jwt header: %w", err)
	}
	if hdr.Kid == "" {
		return nil, errors.New("jwt missing kid")
	}

	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return nil, fmt.Errorf("decode jwt signature: %w", err)
	}

	pub, err := o.getKey(ctx, hdr.Kid, false)
	if err != nil {
		pub, err = o.getKey(ctx, hdr.Kid, true)
		if err != nil {
			return nil, err
		}
	}

	signingInput := []byte(parts[0] + "." + parts[1])
	if err := verifyJWTSig(hdr.Alg, pub, signingInput, sig); err != nil {
		return nil, fmt.Errorf("jwt signature invalid: %w", err)
	}

	payloadBytes, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, fmt.Errorf("decode jwt claims: %w", err)
	}
	var claims struct {
		Iss               string          `json:"iss"`
		Sub               string          `json:"sub"`
		Aud               json.RawMessage `json:"aud"`
		Azp               string          `json:"azp"`
		Exp               int64           `json:"exp"`
		Nbf               int64           `json:"nbf"`
		Nonce             string          `json:"nonce"`
		ACR               string          `json:"acr"`
		AMR               []string        `json:"amr"`
		PreferredUsername string          `json:"preferred_username"`
		Email             string          `json:"email"`
	}
	if err := json.Unmarshal(payloadBytes, &claims); err != nil {
		return nil, fmt.Errorf("unmarshal jwt claims: %w", err)
	}

	if strings.TrimRight(claims.Iss, "/") != strings.TrimRight(o.cfg.IssuerURL, "/") {
		return nil, fmt.Errorf("jwt issuer mismatch: got %q want %q", claims.Iss, o.cfg.IssuerURL)
	}
	nowUnix := o.now().Unix()
	if claims.Exp <= nowUnix-30 {
		return nil, errors.New("jwt expired")
	}
	if claims.Nbf > 0 && claims.Nbf > nowUnix+30 {
		return nil, errors.New("jwt not yet valid")
	}
	if expectedNonce != "" && claims.Nonce != expectedNonce {
		return nil, errors.New("jwt nonce mismatch")
	}
	if requireAudience && !audienceMatches(claims.Aud, claims.Azp, o.cfg.ClientID) {
		return nil, fmt.Errorf("jwt audience mismatch for client %q", o.cfg.ClientID)
	}

	if !containsString(o.cfg.RequiredACRValues, claims.ACR) {
		return nil, fmt.Errorf("insufficient authentication strength acr=%q (required one of %v)", claims.ACR, o.cfg.RequiredACRValues)
	}
	if len(o.cfg.RequiredAMRValues) > 0 && !intersects(o.cfg.RequiredAMRValues, claims.AMR) {
		return nil, fmt.Errorf("insufficient authentication method amr=%v (required one of %v)", claims.AMR, o.cfg.RequiredAMRValues)
	}

	username := claims.PreferredUsername
	if username == "" {
		username = claims.Email
	}
	if username == "" {
		username = claims.Sub
	}

	return &HumanIdentity{
		Subject:  claims.Sub,
		Username: username,
		Email:    claims.Email,
		ACR:      claims.ACR,
		AMR:      claims.AMR,
		Exp:      claims.Exp,
	}, nil
}

func verifyJWTSig(alg string, pub crypto.PublicKey, input, sig []byte) error {
	switch alg {
	case "RS256", "RS384", "RS512":
		rsaPub, ok := pub.(*rsa.PublicKey)
		if !ok {
			return errors.New("key is not RSA")
		}
		var hashType crypto.Hash
		switch alg {
		case "RS256":
			hashType = crypto.SHA256
		case "RS384":
			hashType = crypto.SHA384
		case "RS512":
			hashType = crypto.SHA512
		}
		h := hashType.New()
		h.Write(input)
		return rsa.VerifyPKCS1v15(rsaPub, hashType, h.Sum(nil), sig)
	case "ES256", "ES384":
		ecPub, ok := pub.(*ecdsa.PublicKey)
		if !ok {
			return errors.New("key is not ECDSA")
		}
		var hashType crypto.Hash
		var compLen int
		switch alg {
		case "ES256":
			hashType = crypto.SHA256
			compLen = 32
		case "ES384":
			hashType = crypto.SHA384
			compLen = 48
		}
		if len(sig) != 2*compLen {
			return errors.New("invalid ECDSA signature length")
		}
		h := hashType.New()
		h.Write(input)
		r := new(big.Int).SetBytes(sig[:compLen])
		s := new(big.Int).SetBytes(sig[compLen:])
		if !ecdsa.Verify(ecPub, h.Sum(nil), r, s) {
			return errors.New("ECDSA verification failed")
		}
		return nil
	default:
		return fmt.Errorf("unsupported JWT alg %q", alg)
	}
}

func audienceMatches(rawAud json.RawMessage, azp, clientID string) bool {
	if azp == clientID {
		return true
	}
	var single string
	if err := json.Unmarshal(rawAud, &single); err == nil && single == clientID {
		return true
	}
	var multi []string
	if err := json.Unmarshal(rawAud, &multi); err == nil {
		return containsString(multi, clientID)
	}
	return false
}

func containsString(list []string, target string) bool {
	for _, item := range list {
		if item == target {
			return true
		}
	}
	return false
}

func intersects(a, b []string) bool {
	for _, x := range a {
		if containsString(b, x) {
			return true
		}
	}
	return false
}

func (o *OIDCVerifier) seal(v any) (string, error) {
	plain, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	nonce := make([]byte, o.aead.NonceSize())
	if _, err := rand.Read(nonce); err != nil {
		return "", err
	}
	ct := o.aead.Seal(nonce, nonce, plain, nil)
	return base64.RawURLEncoding.EncodeToString(ct), nil
}

func (o *OIDCVerifier) unseal(token string, dst any) error {
	raw, err := base64.RawURLEncoding.DecodeString(token)
	if err != nil {
		return err
	}
	ns := o.aead.NonceSize()
	if len(raw) <= ns {
		return errors.New("ciphertext too short")
	}
	plain, err := o.aead.Open(nil, raw[:ns], raw[ns:], nil)
	if err != nil {
		return err
	}
	return json.Unmarshal(plain, dst)
}

func randomToken(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func (o *OIDCVerifier) StartAuth(ctx context.Context, r *http.Request, peer *PeerIdentity) (authURL string, stateCookie *http.Cookie, err error) {
	disc, err := o.getDiscovery(ctx)
	if err != nil {
		return "", nil, err
	}
	state, err := randomToken(24)
	if err != nil {
		return "", nil, err
	}
	nonce, err := randomToken(24)
	if err != nil {
		return "", nil, err
	}
	verifier, err := randomToken(32)
	if err != nil {
		return "", nil, err
	}
	h := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(h[:])

	returnTo := r.URL.RequestURI()
	if !strings.HasPrefix(returnTo, "/") || strings.HasPrefix(returnTo, "//") {
		returnTo = "/"
	}

	sealed, err := o.seal(pkceState{
		State:        state,
		Nonce:        nonce,
		CodeVerifier: verifier,
		ReturnTo:     returnTo,
		NodeID:       peer.NodeID,
		Exp:          o.now().Add(10 * time.Minute).Unix(),
	})
	if err != nil {
		return "", nil, err
	}

	redirectURI := o.redirectURI(r)
	q := url.Values{}
	q.Set("response_type", "code")
	q.Set("client_id", o.cfg.ClientID)
	q.Set("redirect_uri", redirectURI)
	q.Set("scope", "openid profile email")
	q.Set("state", state)
	q.Set("nonce", nonce)
	q.Set("code_challenge", challenge)
	q.Set("code_challenge_method", "S256")
	q.Set("acr_values", strings.Join(o.cfg.RequiredACRValues, " "))

	cookie := &http.Cookie{
		Name:     "pits_pep_state",
		Value:    sealed,
		Path:     "/",
		HttpOnly: true,
		Secure:   r.TLS != nil,
		SameSite: http.SameSiteLaxMode,
		MaxAge:   600,
	}
	return disc.AuthorizationEndpoint + "?" + q.Encode(), cookie, nil
}

func (o *OIDCVerifier) CompleteAuth(ctx context.Context, r *http.Request, peer *PeerIdentity) (*HumanIdentity, *http.Cookie, string, error) {
	if errMsg := r.URL.Query().Get("error"); errMsg != "" {
		return nil, nil, "", fmt.Errorf("oidc error %s: %s", errMsg, r.URL.Query().Get("error_description"))
	}
	stateParam := r.URL.Query().Get("state")
	code := r.URL.Query().Get("code")
	if stateParam == "" || code == "" {
		return nil, nil, "", errors.New("missing state or code")
	}

	c, err := r.Cookie("pits_pep_state")
	if err != nil {
		return nil, nil, "", errors.New("missing OIDC state cookie")
	}
	var st pkceState
	if err := o.unseal(c.Value, &st); err != nil {
		return nil, nil, "", fmt.Errorf("invalid OIDC state cookie: %w", err)
	}
	if o.now().Unix() > st.Exp {
		return nil, nil, "", errors.New("OIDC state expired")
	}
	if st.State != stateParam {
		return nil, nil, "", errors.New("OIDC state mismatch")
	}
	if st.NodeID != peer.NodeID {
		return nil, nil, "", errors.New("OIDC flow bound to a different Tailscale node")
	}

	disc, err := o.getDiscovery(ctx)
	if err != nil {
		return nil, nil, "", err
	}

	form := url.Values{}
	form.Set("grant_type", "authorization_code")
	form.Set("code", code)
	form.Set("redirect_uri", o.redirectURI(r))
	form.Set("client_id", o.cfg.ClientID)
	if o.cfg.ClientSecret != "" {
		form.Set("client_secret", o.cfg.ClientSecret)
	}
	form.Set("code_verifier", st.CodeVerifier)

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, disc.TokenEndpoint, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, nil, "", err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	resp, err := o.httpClient.Do(req)
	if err != nil {
		return nil, nil, "", fmt.Errorf("token exchange failed: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 1024))
		return nil, nil, "", fmt.Errorf("token endpoint HTTP %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}

	var tokResp struct {
		IDToken     string `json:"id_token"`
		AccessToken string `json:"access_token"`
	}
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&tokResp); err != nil {
		return nil, nil, "", fmt.Errorf("decode token response: %w", err)
	}
	if tokResp.IDToken == "" {
		return nil, nil, "", errors.New("token response missing id_token")
	}

	ident, err := o.VerifyJWT(ctx, tokResp.IDToken, true, st.Nonce)
	if err != nil {
		return nil, nil, "", fmt.Errorf("id_token verification failed: %w", err)
	}
	ident.NodeID = peer.NodeID
	sessExp := o.now().Add(o.cfg.SessionTTL).Unix()
	if ident.Exp == 0 || sessExp < ident.Exp {
		ident.Exp = sessExp
	}

	sealedSess, err := o.seal(ident)
	if err != nil {
		return nil, nil, "", err
	}
	sessCookie := &http.Cookie{
		Name:     "pits_pep_session",
		Value:    sealedSess,
		Path:     "/",
		HttpOnly: true,
		Secure:   r.TLS != nil,
		SameSite: http.SameSiteLaxMode,
		MaxAge:   int(o.cfg.SessionTTL.Seconds()),
	}
	return ident, sessCookie, st.ReturnTo, nil
}

// VerifySession checks an encrypted session cookie and ensures it is bound to the current Tailscale node.
func (o *OIDCVerifier) VerifySession(r *http.Request, peer *PeerIdentity) (*HumanIdentity, error) {
	c, err := r.Cookie("pits_pep_session")
	if err != nil {
		return nil, errors.New("no session cookie")
	}
	var ident HumanIdentity
	if err := o.unseal(c.Value, &ident); err != nil {
		return nil, fmt.Errorf("invalid session cookie: %w", err)
	}
	if o.now().Unix() >= ident.Exp {
		return nil, errors.New("session expired")
	}
	if ident.NodeID != peer.NodeID {
		return nil, fmt.Errorf("session node mismatch: cookie=%q peer=%q", ident.NodeID, peer.NodeID)
	}
	if !containsString(o.cfg.RequiredACRValues, ident.ACR) {
		return nil, fmt.Errorf("session acr=%q insufficient", ident.ACR)
	}
	return &ident, nil
}

func (o *OIDCVerifier) redirectURI(r *http.Request) string {
	if strings.TrimSpace(o.cfg.ExternalBaseURL) != "" {
		return strings.TrimRight(o.cfg.ExternalBaseURL, "/") + o.cfg.RedirectPath
	}
	scheme := "https"
	if r.TLS == nil {
		scheme = "http"
	}
	return fmt.Sprintf("%s://%s%s", scheme, r.Host, o.cfg.RedirectPath)
}
