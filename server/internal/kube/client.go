// Package kube provides a minimal zero-dependency Kubernetes REST and Watch
// client initialized from a standard kubeconfig file or injected HTTP endpoint.
package kube

import (
	"bufio"
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"go.yaml.in/yaml/v3"
)

// WatchEvent represents one newline-delimited Kubernetes Watch frame.
type WatchEvent struct {
	Type   string          `json:"type"`
	Object json.RawMessage `json:"object"`
}

// K8sStatus captures an API Status payload (for example HTTP 410 Gone inside a Watch ERROR frame).
type K8sStatus struct {
	Kind    string `json:"kind"`
	Status  string `json:"status"`
	Message string `json:"message"`
	Reason  string `json:"reason"`
	Code    int    `json:"code"`
}

// HTTPStatusError signals a non-2xx Kubernetes API response code.
type HTTPStatusError struct {
	Method string
	Path   string
	Code   int
	Body   string
}

func (e *HTTPStatusError) Error() string {
	return fmt.Sprintf("%s %s HTTP %d: %s", e.Method, e.Path, e.Code, e.Body)
}

// Client talks to a Kubernetes API server with bearer token or mTLS credentials.
type Client struct {
	server    string
	token     string
	http      *http.Client
	watchHTTP *http.Client
}

// NewFromKubeconfig builds a Client from a YAML kubeconfig file.
func NewFromKubeconfig(kubeconfigPath string) (*Client, error) {
	raw, err := os.ReadFile(kubeconfigPath)
	if err != nil {
		return nil, err
	}
	return NewFromKubeconfigBytes(raw)
}

// NewFromKubeconfigBytes builds a Client from raw YAML kubeconfig bytes.
func NewFromKubeconfigBytes(raw []byte) (*Client, error) {
	var cfg struct {
		CurrentContext string `yaml:"current-context"`
		Clusters       []struct {
			Name    string `yaml:"name"`
			Cluster struct {
				Server                   string `yaml:"server"`
				TLSServerName            string `yaml:"tls-server-name"`
				CertificateAuthorityData string `yaml:"certificate-authority-data"`
				InsecureSkipTLSVerify    bool   `yaml:"insecure-skip-tls-verify"`
			} `yaml:"cluster"`
		} `yaml:"clusters"`
		Contexts []struct {
			Name    string `yaml:"name"`
			Context struct {
				Cluster string `yaml:"cluster"`
				User    string `yaml:"user"`
			} `yaml:"context"`
		} `yaml:"contexts"`
		Users []struct {
			Name string `yaml:"name"`
			User struct {
				Token                 string `yaml:"token"`
				ClientCertificateData string `yaml:"client-certificate-data"`
				ClientKeyData         string `yaml:"client-key-data"`
			} `yaml:"user"`
		} `yaml:"users"`
	}
	if err := yaml.Unmarshal(raw, &cfg); err != nil {
		return nil, err
	}
	if len(cfg.Clusters) == 0 {
		return nil, fmt.Errorf("kubeconfig contains zero clusters")
	}

	clusterIdx, userIdx := 0, 0
	if cfg.CurrentContext != "" {
		for _, ctx := range cfg.Contexts {
			if ctx.Name == cfg.CurrentContext {
				for i, cl := range cfg.Clusters {
					if cl.Name == ctx.Context.Cluster {
						clusterIdx = i
						break
					}
				}
				for i, u := range cfg.Users {
					if u.Name == ctx.Context.User {
						userIdx = i
						break
					}
				}
				break
			}
		}
	}

	cl := cfg.Clusters[clusterIdx].Cluster
	tlsCfg := &tls.Config{
		MinVersion:         tls.VersionTLS12,
		InsecureSkipVerify: cl.InsecureSkipTLSVerify,
		ServerName:         strings.TrimSpace(cl.TLSServerName),
	}
	if caB64 := strings.TrimSpace(cl.CertificateAuthorityData); caB64 != "" {
		if caPEM, derr := base64.StdEncoding.DecodeString(caB64); derr == nil {
			pool := x509.NewCertPool()
			pool.AppendCertsFromPEM(caPEM)
			tlsCfg.RootCAs = pool
		}
	}
	var token string
	if userIdx < len(cfg.Users) {
		u := cfg.Users[userIdx].User
		token = strings.TrimSpace(u.Token)
		if crtB64 := strings.TrimSpace(u.ClientCertificateData); crtB64 != "" {
			crtPEM, err1 := base64.StdEncoding.DecodeString(crtB64)
			keyPEM, err2 := base64.StdEncoding.DecodeString(strings.TrimSpace(u.ClientKeyData))
			if err1 == nil && err2 == nil {
				if pair, perr := tls.X509KeyPair(crtPEM, keyPEM); perr == nil {
					tlsCfg.Certificates = []tls.Certificate{pair}
				}
			}
		}
	}
	transport := &http.Transport{TLSClientConfig: tlsCfg}
	return NewFromTransport(cl.Server, token, transport), nil
}

// ServerURL returns the normalized API server base URL.
func (c *Client) ServerURL() string {
	return c.server
}

// NewFromTransport constructs a Client using a custom base URL and RoundTripper (useful for tests).
func NewFromTransport(serverURL, bearerToken string, transport http.RoundTripper) *Client {
	return &Client{
		server: strings.TrimRight(serverURL, "/"),
		token:  bearerToken,
		http: &http.Client{
			Timeout:   15 * time.Second,
			Transport: transport,
		},
		// Watch streams rely on context cancellation / server timeoutSeconds, not client-side hard deadline.
		watchHTTP: &http.Client{
			Transport: transport,
		},
	}
}

func (c *Client) setAuth(req *http.Request) {
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}
}

// GetJSON performs a synchronous GET request and decodes JSON into out.
func (c *Client) GetJSON(ctx context.Context, path string, out any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.server+path, nil)
	if err != nil {
		return err
	}
	c.setAuth(req)
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return &HTTPStatusError{Method: http.MethodGet, Path: path, Code: resp.StatusCode, Body: strings.TrimSpace(string(b))}
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// PatchJSON performs a merge-patch request against the given API path.
func (c *Client) PatchJSON(ctx context.Context, path, mergeBody string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPatch, c.server+path, bytes.NewBufferString(mergeBody))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/merge-patch+json")
	c.setAuth(req)
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return &HTTPStatusError{Method: http.MethodPatch, Path: path, Code: resp.StatusCode, Body: strings.TrimSpace(string(b))}
	}
	return nil
}

// Watch streams Kubernetes WatchEvent frames until EOF, handler error, or context cancellation.
func (c *Client) Watch(ctx context.Context, path string, onEvent func(WatchEvent) error) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.server+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "application/json")
	c.setAuth(req)
	resp, err := c.watchHTTP.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return &HTTPStatusError{Method: http.MethodGet, Path: path, Code: resp.StatusCode, Body: strings.TrimSpace(string(b))}
	}
	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 64*1024), 4*1024*1024)
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		var ev WatchEvent
		if err := json.Unmarshal(line, &ev); err != nil {
			return fmt.Errorf("decode watch frame: %w", err)
		}
		if err := onEvent(ev); err != nil {
			return err
		}
	}
	return scanner.Err()
}
