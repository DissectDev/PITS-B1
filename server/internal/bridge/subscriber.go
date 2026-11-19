package bridge

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"strings"
	"time"

	"pits.io/server/internal/kube"
)

const (
	defaultRoutingNamespace = "edge"
	defaultRoutingConfigMap = "pits-core-routing-table"
	defaultEdgeRouterName   = "pits-core"
)

// RoutingSubscriptionStatus exposes live EdgeRouter watch health & active semantic routing table.
type RoutingSubscriptionStatus struct {
	Subscribed      bool                  `json:"subscribed"`
	Source          string                `json:"source"`
	ResourceVersion string                `json:"resourceVersion,omitempty"`
	Hash            string                `json:"hash,omitempty"`
	Generation      int64                 `json:"generation,omitempty"`
	SyncCount       int64                 `json:"syncCount"`
	LastSyncAt      *time.Time            `json:"lastSyncAt,omitempty"`
	LastError       string                `json:"lastError,omitempty"`
	RoutingTable    *SemanticRoutingTable `json:"routingTable,omitempty"`
}

// StartSubscriber begins a resilient Kubernetes Get+Watch loop subscribing to
// EdgeRouter's semantic routing table (`ConfigMap edge/pits-core-routing-table`,
// with EdgeRouter `.status.routingTable` fallback during bootstrap).
func (t *Tunnel) StartSubscriber(ctx context.Context, kc *kube.Client) {
	if kc == nil {
		return
	}
	go t.runSubscriberLoop(ctx, kc)
}

func (t *Tunnel) runSubscriberLoop(ctx context.Context, kc *kube.Client) {
	backoff := time.Second
	const maxBackoff = 15 * time.Second

	for {
		if ctx.Err() != nil {
			return
		}
		rv, err := t.syncAndWatchOnce(ctx, kc)
		if ctx.Err() != nil {
			return
		}
		if err != nil {
			t.recordSyncError(err.Error())
			log.Printf("bridge: EdgeRouter routing-table watch retry in %s (rv=%s): %v", backoff, rv, err)
			select {
			case <-ctx.Done():
				return
			case <-time.After(backoff):
			}
			backoff *= 2
			if backoff > maxBackoff {
				backoff = maxBackoff
			}
		} else {
			backoff = time.Second
		}
	}
}

func (t *Tunnel) syncAndWatchOnce(ctx context.Context, kc *kube.Client) (string, error) {
	cmPath := fmt.Sprintf("/api/v1/namespaces/%s/configmaps/%s", defaultRoutingNamespace, defaultRoutingConfigMap)

	var rawCM json.RawMessage
	err := kc.GetJSON(ctx, cmPath, &rawCM)
	if err == nil {
		table, rv, hash, perr := ParseConfigMapJSON(rawCM)
		if perr != nil {
			return rv, perr
		}
		t.applyRoutingSnapshot("k8s-watch:configmap/edge/pits-core-routing-table", rv, hash, table)
		return rv, t.watchRoutingConfigMap(ctx, kc, rv)
	}

	// Graceful bootstrap fallback: if ConfigMap isn't synced yet, try EdgeRouter status.routingTable.
	var statusErr *kube.HTTPStatusError
	if errors.As(err, &statusErr) && statusErr.Code == http.StatusNotFound {
		erPath := fmt.Sprintf("/apis/platform.edge.router/v1alpha1/namespaces/%s/edgerouters/%s", defaultRoutingNamespace, defaultEdgeRouterName)
		var rawER json.RawMessage
		if erErr := kc.GetJSON(ctx, erPath, &rawER); erErr == nil {
			if table, rv, perr := ParseEdgeRouterJSON(rawER); perr == nil {
				t.applyRoutingSnapshot("k8s-get:edgerouter/edge/pits-core", rv, table.Hash, table)
			}
		}
	}
	return "", err
}

func (t *Tunnel) watchRoutingConfigMap(ctx context.Context, kc *kube.Client, initialRV string) error {
	rv := initialRV
	for {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		q := url.Values{}
		q.Set("fieldSelector", "metadata.name="+defaultRoutingConfigMap)
		q.Set("watch", "true")
		q.Set("allowWatchBookmarks", "true")
		q.Set("timeoutSeconds", "300")
		if rv != "" {
			q.Set("resourceVersion", rv)
		}
		watchPath := fmt.Sprintf("/api/v1/namespaces/%s/configmaps?%s", defaultRoutingNamespace, q.Encode())

		err := kc.Watch(ctx, watchPath, func(ev kube.WatchEvent) error {
			switch strings.ToUpper(ev.Type) {
			case "ADDED", "MODIFIED":
				table, nextRV, hash, perr := ParseConfigMapJSON(ev.Object)
				if perr != nil {
					return perr
				}
				if nextRV != "" {
					rv = nextRV
				}
				t.applyRoutingSnapshot("k8s-watch:configmap/edge/pits-core-routing-table", nextRV, hash, table)
			case "BOOKMARK":
				var meta struct {
					Metadata struct {
						ResourceVersion string `json:"resourceVersion"`
					} `json:"metadata"`
				}
				if json.Unmarshal(ev.Object, &meta) == nil && meta.Metadata.ResourceVersion != "" {
					rv = meta.Metadata.ResourceVersion
				}
			case "DELETED":
				return fmt.Errorf("ConfigMap %s/%s deleted; triggering re-sync", defaultRoutingNamespace, defaultRoutingConfigMap)
			case "ERROR":
				var st kube.K8sStatus
				if json.Unmarshal(ev.Object, &st) == nil {
					if st.Code == http.StatusGone || strings.EqualFold(st.Reason, "Expired") {
						rv = ""
						return fmt.Errorf("watch resourceVersion expired (410 Gone)")
					}
					return fmt.Errorf("watch error status %d: %s", st.Code, st.Message)
				}
				return fmt.Errorf("watch ERROR frame received")
			}
			return nil
		})
		if err != nil {
			return err
		}
		// Clean 300s server watch timeout: continue immediately from last resourceVersion.
	}
}
