package api

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"runtime"
	"strings"
	"time"
)

// HostProvider abstracts host sysfs/PCI/VFIO preflight and recovery operations.
type HostProvider interface {
	Audit() HostAuditReport
	RecoverRouter(ctx context.Context, req RouterRecoveryRequest) (*RouterRecoveryResult, error)
}

// BtrfsProvider abstracts pits-data Btrfs pool status and maintenance.
type BtrfsProvider interface {
	Status() BtrfsStatus
	Maintain(op string) (*BtrfsMaintenanceResult, error)
}

// SANProvider abstracts NVMe/TCP target inspection, provisioning, and growth.
type SANProvider interface {
	Inspect(spec SANDeviceSpec) SANTargetStatus
	Provision(req SANProvisionRequest) (*SANTargetStatus, error)
	Expand(req SANExpandRequest) (*SANNamespaceStatus, error)
}

// VPPProvider abstracts GoVPP binary socket inspection and packet traces.
type VPPProvider interface {
	Inspect(ctx context.Context) (*VPPInspectReport, error)
	Trace(ctx context.Context, req VPPTraceRequest) (*VPPTraceResult, error)
	ArmTrace(ctx context.Context, count int) error
}

// PXEProvider abstracts Hook hardware inventory reports and boot asset hashes.
type PXEProvider interface {
	ListReports() (map[string]json.RawMessage, error)
	SaveReport(deviceID, expectedMAC string, payload []byte) error
	VerifyAssets(req PXEVerifyAssetsRequest) (*PXEVerifyAssetsResult, error)
}

// ConnectTunnelProvider handles HTTP CONNECT tunnels to Lab VLAN 20 endpoints.
type ConnectTunnelProvider interface {
	ServeConnect(w http.ResponseWriter, r *http.Request)
}

// Handler wires all pits-agent domain controllers onto one authenticated HTTP/mTLS mux.
type Handler struct {
	Version     string
	BearerToken string
	Host        HostProvider
	Btrfs       BtrfsProvider
	SAN         SANProvider
	VPP         VPPProvider
	PXE         PXEProvider
	Bridge      ConnectTunnelProvider
	mux         *http.ServeMux
}

// NewHandler builds a ready-to-serve pits-agent HTTP handler.
func NewHandler(h *Handler) http.Handler {
	mux := http.NewServeMux()
	h.mux = mux

	mux.HandleFunc("GET /v1/health", h.handleHealth)
	mux.HandleFunc("GET /v1/host/audit", h.handleHostAudit)
	mux.HandleFunc("POST /v1/router/recover", h.handleRouterRecover)
	mux.HandleFunc("GET /v1/vpp/inspect", h.handleVPPInspect)
	mux.HandleFunc("POST /v1/vpp/trace/arm", h.handleVPPArmTrace)
	mux.HandleFunc("POST /v1/vpp/trace", h.handleVPPTrace)
	mux.HandleFunc("GET /v1/btrfs/status", h.handleBtrfsStatus)
	mux.HandleFunc("POST /v1/btrfs/maintain", h.handleBtrfsMaintain)
	mux.HandleFunc("POST /v1/san/inspect", h.handleSANInspect)
	mux.HandleFunc("POST /v1/san/provision", h.handleSANProvision)
	mux.HandleFunc("POST /v1/san/expand", h.handleSANExpand)
	mux.HandleFunc("GET /v1/pxe/inventory", h.handlePXEInventoryList)
	mux.HandleFunc("POST /v1/pxe/inventory/{device}", h.handlePXEInventorySave)
	mux.HandleFunc("POST /v1/pxe/assets/verify", h.handlePXEVerifyAssets)

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !h.authorize(r) {
			writeError(w, http.StatusUnauthorized, "unauthorized")
			return
		}
		if r.Method == http.MethodConnect {
			if h.Bridge == nil {
				writeError(w, http.StatusNotImplemented, "lab bridge is disabled")
				return
			}
			h.Bridge.ServeConnect(w, r)
			return
		}
		mux.ServeHTTP(w, r)
	})
}

func (h *Handler) authorize(r *http.Request) bool {
	if h.BearerToken == "" {
		return true
	}
	got := strings.TrimSpace(strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer"))
	return got == h.BearerToken
}

func (h *Handler) handleHealth(w http.ResponseWriter, _ *http.Request) {
	hostname, _ := os.Hostname()
	ver := h.Version
	if ver == "" {
		ver = "1.0.0"
	}
	writeJSON(w, http.StatusOK, ServerHealth{
		Version:    ver,
		Protocol:   ProtocolVersion,
		Hostname:   hostname,
		OS:         runtime.GOOS,
		Arch:       runtime.GOARCH,
		ServerTime: time.Now().UTC(),
		Capabilities: []string{
			"host.audit",
			"router.vfio-recover",
			"vpp.govpp-inspect",
			"btrfs.status-trim-scrub",
			"san.nvmet-gpt4k",
			"pxe.inventory-assets",
			"lab.http-connect-bridge",
		},
	})
}

func (h *Handler) handleHostAudit(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, h.Host.Audit())
}

func (h *Handler) handleRouterRecover(w http.ResponseWriter, r *http.Request) {
	var req RouterRecoveryRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.Host.RecoverRouter(r.Context(), req)
	if err != nil {
		if res != nil {
			writeJSON(w, http.StatusConflict, res)
			return
		}
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (h *Handler) handleVPPInspect(w http.ResponseWriter, r *http.Request) {
	rep, err := h.VPP.Inspect(r.Context())
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, rep)
}

func (h *Handler) handleVPPArmTrace(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Count int `json:"count"`
	}
	_ = decodeJSON(r, &req)
	if err := h.VPP.ArmTrace(r.Context(), req.Count); err != nil {
		writeError(w, http.StatusServiceUnavailable, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"armed": true, "count": req.Count})
}

func (h *Handler) handleVPPTrace(w http.ResponseWriter, r *http.Request) {
	var req VPPTraceRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.VPP.Trace(r.Context(), req)
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (h *Handler) handleBtrfsStatus(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, h.Btrfs.Status())
}

func (h *Handler) handleBtrfsMaintain(w http.ResponseWriter, r *http.Request) {
	var req BtrfsMaintenanceRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.Btrfs.Maintain(req.Operation)
	if err != nil {
		writeError(w, http.StatusConflict, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (h *Handler) handleSANInspect(w http.ResponseWriter, r *http.Request) {
	var spec SANDeviceSpec
	if err := decodeJSON(r, &spec); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, h.SAN.Inspect(spec))
}

func (h *Handler) handleSANProvision(w http.ResponseWriter, r *http.Request) {
	var req SANProvisionRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.SAN.Provision(req)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (h *Handler) handleSANExpand(w http.ResponseWriter, r *http.Request) {
	var req SANExpandRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.SAN.Expand(req)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func (h *Handler) handlePXEInventoryList(w http.ResponseWriter, _ *http.Request) {
	reports, err := h.PXE.ListReports()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, reports)
}

func (h *Handler) handlePXEInventorySave(w http.ResponseWriter, r *http.Request) {
	dev := r.PathValue("device")
	body, err := io.ReadAll(io.LimitReader(r.Body, 1024*1024+1))
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	mac := r.Header.Get("X-Expected-MAC")
	if err := h.PXE.SaveReport(dev, mac, body); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{"saved": true, "deviceID": dev})
}

func (h *Handler) handlePXEVerifyAssets(w http.ResponseWriter, r *http.Request) {
	var req PXEVerifyAssetsRequest
	if err := decodeJSON(r, &req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	res, err := h.PXE.VerifyAssets(req)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, res)
}

func decodeJSON(r *http.Request, dst any) error {
	defer r.Body.Close()
	dec := json.NewDecoder(io.LimitReader(r.Body, 4<<20))
	return dec.Decode(dst)
}

func writeJSON(w http.ResponseWriter, status int, payload any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(payload)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}
