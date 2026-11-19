package pep

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// PeerIdentity represents the verified Tailscale machine and user posture from tailscaled Whois.
type PeerIdentity struct {
	NodeID    string
	NodeName  string
	TailnetIP string
	LoginName string
	Tags      []string
	OS        string
	KeyExpiry time.Time
	Caps      map[string][]json.RawMessage
}

// TailscalePolicy defines machine identity and device posture requirements for Mode A PEP.
type TailscalePolicy struct {
	RequireUserOwned bool
	AllowedTags      map[string]bool
	AllowedUsers     map[string]bool
	AllowedOS        map[string]bool
	RequiredCap      string
}

// TailscaleVerifier resolves and validates a request source address against tailscaled Whois.
type TailscaleVerifier interface {
	VerifyPeer(ctx context.Context, remoteAddr string) (*PeerIdentity, error)
}

type whoisResponse struct {
	Node *struct {
		ID        int64      `json:"ID"`
		StableID  string     `json:"StableID"`
		Name      string     `json:"Name"`
		Addresses []string   `json:"Addresses"`
		Tags      []string   `json:"Tags"`
		KeyExpiry *time.Time `json:"KeyExpiry"`
		Hostinfo  *struct {
			OS       string `json:"OS"`
			Hostname string `json:"Hostname"`
		} `json:"Hostinfo"`
	} `json:"Node"`
	UserProfile *struct {
		ID          int64  `json:"ID"`
		LoginName   string `json:"LoginName"`
		DisplayName string `json:"DisplayName"`
	} `json:"UserProfile"`
	CapMap map[string][]json.RawMessage `json:"CapMap"`
}

// LocalAPIVerifier calls tailscaled LocalAPI /localapi/v0/whois over a Unix socket (or mock URL).
type LocalAPIVerifier struct {
	client  *http.Client
	baseURL string
	policy  TailscalePolicy
	now     func() time.Time
}

// NewLocalAPIVerifier constructs a Tailscale LocalAPI Whois verifier for the given socket path.
func NewLocalAPIVerifier(socketPath string, policy TailscalePolicy) *LocalAPIVerifier {
	tr := &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", socketPath)
		},
	}
	return &LocalAPIVerifier{
		client:  &http.Client{Transport: tr, Timeout: 3 * time.Second},
		baseURL: "http://local-tailscaled.sock",
		policy:  policy,
		now:     time.Now,
	}
}

// NewHTTPWhoisVerifier constructs a Whois verifier against an HTTP endpoint (used in unit tests).
func NewHTTPWhoisVerifier(baseURL string, client *http.Client, policy TailscalePolicy) *LocalAPIVerifier {
	if client == nil {
		client = &http.Client{Timeout: 3 * time.Second}
	}
	return &LocalAPIVerifier{
		client:  client,
		baseURL: strings.TrimRight(baseURL, "/"),
		policy:  policy,
		now:     time.Now,
	}
}

func (v *LocalAPIVerifier) VerifyPeer(ctx context.Context, remoteAddr string) (*PeerIdentity, error) {
	if strings.TrimSpace(remoteAddr) == "" {
		return nil, errors.New("empty remote address")
	}

	reqURL := fmt.Sprintf("%s/localapi/v0/whois?addr=%s", v.baseURL, url.QueryEscape(remoteAddr))
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, reqURL, nil)
	if err != nil {
		return nil, fmt.Errorf("build whois request: %w", err)
	}
	resp, err := v.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("tailscaled whois failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
		return nil, fmt.Errorf("tailscaled whois status %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}

	var wr whoisResponse
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&wr); err != nil {
		return nil, fmt.Errorf("decode whois response: %w", err)
	}
	if wr.Node == nil {
		return nil, errors.New("whois response missing node")
	}

	now := v.now()
	var keyExpiry time.Time
	if wr.Node.KeyExpiry != nil && !wr.Node.KeyExpiry.IsZero() {
		keyExpiry = *wr.Node.KeyExpiry
		if !now.Before(keyExpiry) {
			return nil, fmt.Errorf("node key expired at %s", keyExpiry.Format(time.RFC3339))
		}
	}

	loginName := ""
	if wr.UserProfile != nil {
		loginName = strings.TrimSpace(wr.UserProfile.LoginName)
	}

	nodeOS := ""
	if wr.Node.Hostinfo != nil {
		nodeOS = strings.ToLower(strings.TrimSpace(wr.Node.Hostinfo.OS))
	}

	hostIP, _, err := net.SplitHostPort(remoteAddr)
	if err != nil {
		hostIP = remoteAddr
	}

	nodeID := wr.Node.StableID
	if nodeID == "" {
		nodeID = fmt.Sprintf("%d", wr.Node.ID)
	}

	peer := &PeerIdentity{
		NodeID:    nodeID,
		NodeName:  strings.TrimSuffix(wr.Node.Name, "."),
		TailnetIP: hostIP,
		LoginName: loginName,
		Tags:      append([]string(nil), wr.Node.Tags...),
		OS:        nodeOS,
		KeyExpiry: keyExpiry,
		Caps:      wr.CapMap,
	}

	if err := v.enforcePolicy(peer); err != nil {
		return nil, err
	}
	return peer, nil
}

func (v *LocalAPIVerifier) enforcePolicy(peer *PeerIdentity) error {
	isTagged := len(peer.Tags) > 0 || strings.EqualFold(peer.LoginName, "tagged-devices")
	if isTagged {
		tagAllowed := false
		for _, t := range peer.Tags {
			if v.policy.AllowedTags[t] {
				tagAllowed = true
				break
			}
		}
		if v.policy.RequireUserOwned && !tagAllowed {
			return fmt.Errorf("tagged device (%v) rejected: user-owned device required", peer.Tags)
		}
		if len(v.policy.AllowedTags) > 0 && !tagAllowed {
			return fmt.Errorf("device tags (%v) not in allowed tags", peer.Tags)
		}
	}

	if len(v.policy.AllowedUsers) > 0 && !isTagged {
		if !v.policy.AllowedUsers[peer.LoginName] {
			return fmt.Errorf("tailscale user %q not allowed", peer.LoginName)
		}
	}

	if len(v.policy.AllowedOS) > 0 {
		if !v.policy.AllowedOS[peer.OS] {
			return fmt.Errorf("device OS %q not allowed", peer.OS)
		}
	}

	if v.policy.RequiredCap != "" {
		vals, ok := peer.Caps[v.policy.RequiredCap]
		if !ok || len(vals) == 0 {
			return fmt.Errorf("missing required tailscale capability %q", v.policy.RequiredCap)
		}
	}
	return nil
}
