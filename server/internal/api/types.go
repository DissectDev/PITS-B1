// Package api defines the cross-language wire schemas shared between the Rust
// PITS client (pitsctl) and the Go node control server (pits-agent) on pits-core.
package api

import "time"

const (
	ProtocolVersion   = "v1"
	ExpectedHostName  = "pits-core"
	ManagementNIC     = "mgmt0"
	WANRouterPCI      = "0000:5b:00.0"
	LANRouterVFPCI    = "0000:03:02.0"
	SecondTrunkVFPCI  = "0000:03:0a.0"
	ExpectedBtrfsUUID = "777b7660-ef8e-421a-ac60-d6c4cb2fea39"
	DefaultBtrfsMount = "/srv/pits-storage/tank"
	DefaultSANRoot    = "/srv/pits-storage/tank/@data/san"
	DefaultConfigfs   = "/sys/kernel/config/nvmet"
	DefaultVPPSocket  = "/run/vpp/api.sock"
	NQNPrefix         = "nqn.2026-11.arpa.home.pits:"
	HostNQNPrefix     = "nqn.2026-11.arpa.home.pits:host-"
)

// ServerHealth reports pits-agent daemon readiness and enabled capabilities.
type ServerHealth struct {
	Version      string    `json:"version"`
	Protocol     string    `json:"protocol"`
	Hostname     string    `json:"hostname"`
	OS           string    `json:"os"`
	Arch         string    `json:"arch"`
	ServerTime   time.Time `json:"serverTime"`
	Capabilities []string  `json:"capabilities"`
}

// PCIDeviceBinding describes one host PCI function and its bound kernel driver.
type PCIDeviceBinding struct {
	Address      string `json:"address"`
	Driver       string `json:"driver"`
	VendorID     string `json:"vendorId,omitempty"`
	DeviceID     string `json:"deviceId,omitempty"`
	IOMMUGroup   string `json:"iommuGroup,omitempty"`
	PhysicalFunc string `json:"physicalFunc,omitempty"`
	Role         string `json:"role,omitempty"`
}

// TrunkInterfaceStatus reports a physical SR-IOV NIC on pits-core.
type TrunkInterfaceStatus struct {
	Name      string `json:"name"`
	OperState string `json:"operState"`
	NumVFs    int    `json:"numVfs"`
	MAC       string `json:"mac,omitempty"`
	SpeedMbps int    `json:"speedMbps,omitempty"`
}

// HostAuditReport replaces scripts/preflight-pits.sh with a typed host snapshot.
type HostAuditReport struct {
	CollectedAt       time.Time              `json:"collectedAt"`
	Hostname          string                 `json:"hostname"`
	HostVerified      bool                   `json:"hostVerified"`
	ManagementNIC     string                 `json:"managementNic"`
	ManagementPCI     string                 `json:"managementPci"`
	ManagementAddress string                 `json:"managementAddress"`
	DefaultRoute      string                 `json:"defaultRoute"`
	Hugepages2Mi      int                    `json:"hugepages2Mi"`
	LogicalCPUs       int                    `json:"logicalCpus"`
	Trunks            []TrunkInterfaceStatus `json:"trunks"`
	PCIBindings       []PCIDeviceBinding     `json:"pciBindings"`
	Warnings          []string               `json:"warnings,omitempty"`
}

// RouterRecoveryRequest configures safe VFIO + device plugin + gateway recovery.
type RouterRecoveryRequest struct {
	RefreshDevicePlugins bool `json:"refreshDevicePlugins"`
	CleanupPerfNetns     bool `json:"cleanupPerfNetns"`
	WaitReadySeconds     int  `json:"waitReadySeconds"`
}

// RouterRecoveryResult replaces Restore-Router.cmd and scripts/restore-router.sh.
type RouterRecoveryResult struct {
	CompletedAt          time.Time          `json:"completedAt"`
	ManagementPCIGuarded string             `json:"managementPciGuarded"`
	ReboundDevices       []PCIDeviceBinding `json:"reboundDevices"`
	PerfNetnsCleaned     bool               `json:"perfNetnsCleaned"`
	PluginsRefreshed     []string           `json:"pluginsRefreshed,omitempty"`
	GatewayReady         bool               `json:"gatewayReady"`
	RouterPhase          string             `json:"routerPhase"`
	ObservedGeneration   int64              `json:"observedGeneration"`
	Message              string             `json:"message"`
}

// VPPInspectReport captures live VPP binary API configuration state.
type VPPInspectReport struct {
	CollectedAt time.Time       `json:"collectedAt"`
	SocketPath  string          `json:"socketPath"`
	Version     string          `json:"version,omitempty"`
	Interfaces  []VPPInterface  `json:"interfaces"`
	ACLs        []VPPACL        `json:"acls"`
	Bindings    []VPPACLBinding `json:"bindings"`
	NATOutputs  []string        `json:"natOutputs"`
	NATStatic   []string        `json:"natStatic"`
	Routes      []string        `json:"routes"`
	RawSections map[string]int  `json:"rawSections,omitempty"`
}

type VPPInterface struct {
	Index     uint32   `json:"index"`
	Name      string   `json:"name"`
	AdminUp   bool     `json:"adminUp"`
	LinkUp    bool     `json:"linkUp"`
	MAC       string   `json:"mac,omitempty"`
	MTU       uint32   `json:"mtu,omitempty"`
	Addresses []string `json:"addresses,omitempty"`
	VRFID     uint32   `json:"vrfId"`
}

type VPPACL struct {
	Index uint32   `json:"index"`
	Tag   string   `json:"tag"`
	Rules []string `json:"rules"`
}

type VPPACLBinding struct {
	InterfaceIndex uint32   `json:"interfaceIndex"`
	InterfaceName  string   `json:"interfaceName,omitempty"`
	InputACLs      []uint32 `json:"inputAcls,omitempty"`
	OutputACLs     []uint32 `json:"outputAcls,omitempty"`
}

// VPPTraceRequest captures packets on VPP dpdk-input to prove ACL/NAT behavior.
type VPPTraceRequest struct {
	PacketCount int    `json:"packetCount"`
	FilterIP    string `json:"filterIp,omitempty"`
	ClearAfter  bool   `json:"clearAfter"`
}

// VPPTraceResult reports packet trace matches and ACL drop confirmations.
type VPPTraceResult struct {
	CapturedAt   time.Time `json:"capturedAt"`
	TotalPackets int       `json:"totalPackets"`
	MatchedDrops int       `json:"matchedDrops"`
	Excerpt      string    `json:"excerpt"`
}

// BtrfsDeviceStats tracks Btrfs per-device error counters.
type BtrfsDeviceStats struct {
	Device              string `json:"device"`
	WriteIOErrs         uint64 `json:"writeIoErrs"`
	ReadIOErrs          uint64 `json:"readIoErrs"`
	FlushIOErrs         uint64 `json:"flushIoErrs"`
	CorruptionErrs      uint64 `json:"corruptionErrs"`
	GenerationErrs      uint64 `json:"generationErrs"`
	HistoricalMediaNote string `json:"historicalMediaNote,omitempty"`
}

// BtrfsStatus reports pits-data Btrfs pool health, capacity, and subvolumes.
type BtrfsStatus struct {
	CheckedAt      time.Time          `json:"checkedAt"`
	MountPath      string             `json:"mountPath"`
	Mounted        bool               `json:"mounted"`
	FilesystemUUID string             `json:"filesystemUuid"`
	UUIDVerified   bool               `json:"uuidVerified"`
	TotalBytes     uint64             `json:"totalBytes"`
	UsedBytes      uint64             `json:"usedBytes"`
	FreeBytes      uint64             `json:"freeBytes"`
	Subvolumes     []string           `json:"subvolumes"`
	Devices        []BtrfsDeviceStats `json:"devices"`
	LastTrimWeek   string             `json:"lastTrimWeek,omitempty"`
	LastScrubMonth string             `json:"lastScrubMonth,omitempty"`
	Healthy        bool               `json:"healthy"`
}

type BtrfsMaintenanceRequest struct {
	Operation string `json:"operation"` // "trim" or "scrub"
}

type BtrfsMaintenanceResult struct {
	Operation   string    `json:"operation"`
	CompletedAt time.Time `json:"completedAt"`
	Summary     string    `json:"summary"`
}

// SANNamespaceSpec defines one NVMe/TCP namespace backing image.
type SANNamespaceSpec struct {
	ID            int    `json:"id" yaml:"id"`
	Name          string `json:"name" yaml:"name"`
	Bytes         int64  `json:"bytes" yaml:"bytes"`
	InitSystemGPT bool   `json:"initSystemGpt,omitempty" yaml:"initSystemGpt,omitempty"`
}

// SANDeviceSpec defines the enrolled NVMe/TCP target contract for a diskless node.
type SANDeviceSpec struct {
	DeviceID     string             `json:"deviceID" yaml:"deviceID"`
	PXEMAC       string             `json:"pxeMAC" yaml:"pxeMAC"`
	ProductUUID  string             `json:"productUUID" yaml:"productUUID"`
	SubsystemNQN string             `json:"subsystemNQN" yaml:"subsystemNQN"`
	HostNQN      string             `json:"hostNQN" yaml:"hostNQN"`
	Address      string             `json:"address" yaml:"address"`
	Port         int                `json:"port" yaml:"port"`
	ConfigfsPort int                `json:"configfsPort" yaml:"configfsPort"`
	VMStore      bool               `json:"vmStore,omitempty" yaml:"vmStore,omitempty"`
	Namespaces   []SANNamespaceSpec `json:"namespaces" yaml:"namespaces"`
}

type SANNamespaceStatus struct {
	ID            int      `json:"id"`
	Name          string   `json:"name"`
	BackingFile   string   `json:"backingFile"`
	FileExists    bool     `json:"fileExists"`
	ActualBytes   int64    `json:"actualBytes"`
	DeviceUUID    string   `json:"deviceUuid"`
	ExpectedWWID  string   `json:"expectedWwid"`
	Enabled       bool     `json:"enabled"`
	GPTVerified   bool     `json:"gptVerified,omitempty"`
	GPTPartitions []string `json:"gptPartitions,omitempty"`
}

type SANTargetStatus struct {
	DeviceID        string               `json:"deviceID"`
	SubsystemNQN    string               `json:"subsystemNQN"`
	SubsystemExists bool                 `json:"subsystemExists"`
	Model           string               `json:"model,omitempty"`
	AllowAnyHost    bool                 `json:"allowAnyHost"`
	AllowedHosts    []string             `json:"allowedHosts"`
	ListenerActive  bool                 `json:"listenerActive"`
	ListenerAddress string               `json:"listenerAddress,omitempty"`
	ListenerPort    int                  `json:"listenerPort,omitempty"`
	NOCOWVerified   bool                 `json:"nocowVerified"`
	Namespaces      []SANNamespaceStatus `json:"namespaces"`
	Healthy         bool                 `json:"healthy"`
	Issues          []string             `json:"issues,omitempty"`
}

type SANProvisionRequest struct {
	Spec              SANDeviceSpec `json:"spec"`
	DryRun            bool          `json:"dryRun"`
	ConfigureListener bool          `json:"configureListener"`
	HotAddOnly        bool          `json:"hotAddOnly"`
}

type SANExpandRequest struct {
	DeviceID    string `json:"deviceID"`
	NamespaceID int    `json:"namespaceID"`
	NewBytes    int64  `json:"newBytes"`
	DryRun      bool   `json:"dryRun"`
}

// PXEAssetInfo describes a staged netboot kernel or initramfs file.
type PXEAssetInfo struct {
	DeviceID  string    `json:"deviceID"`
	Name      string    `json:"name"`
	Path      string    `json:"path"`
	SizeBytes int64     `json:"sizeBytes"`
	SHA256    string    `json:"sha256"`
	UpdatedAt time.Time `json:"updatedAt"`
}

// PXEVerifyAssetsRequest checksstaged vmlinuz/initramfs checksums on pits-core.
type PXEVerifyAssetsRequest struct {
	DeviceID  string            `json:"deviceID"`
	Checksums map[string]string `json:"checksums"`
}

type PXEVerifyAssetsResult struct {
	DeviceID string            `json:"deviceID"`
	Verified bool              `json:"verified"`
	Assets   []PXEAssetInfo    `json:"assets"`
	Mismatch map[string]string `json:"mismatch,omitempty"`
}
