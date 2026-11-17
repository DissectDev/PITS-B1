package san

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strconv"
	"strings"

	"pits.io/server/internal/api"
)

var (
	deviceIDPattern = regexp.MustCompile(`^netboot-\d{3}$`)
	macPattern      = regexp.MustCompile(`^(?:[0-9a-f]{2}:){5}[0-9a-f]{2}$`)
	nsNamePattern   = regexp.MustCompile(`^[a-z][a-z0-9-]{0,31}$`)
)

// Manager reconciles NVMe/TCP subsystems, host ACLs, namespaces, ports, and
// Btrfs backing files without Python or shell scripts.
type Manager struct {
	SANRoot      string
	ConfigfsRoot string
	RequireBtrfs bool
}

// NewManager creates a SAN Manager using default pits-core paths when empty.
func NewManager(sanRoot, configfsRoot string, requireBtrfs bool) *Manager {
	if sanRoot == "" {
		sanRoot = api.DefaultSANRoot
	}
	if configfsRoot == "" {
		configfsRoot = api.DefaultConfigfs
	}
	return &Manager{
		SANRoot:      sanRoot,
		ConfigfsRoot: configfsRoot,
		RequireBtrfs: requireBtrfs,
	}
}

// ValidateDeviceSpec enforces all PITS SAN contract invariants.
func ValidateDeviceSpec(spec api.SANDeviceSpec) error {
	if !deviceIDPattern.MatchString(spec.DeviceID) {
		return fmt.Errorf("invalid enrolled deviceID %q (expected netboot-NNN)", spec.DeviceID)
	}
	if !macPattern.MatchString(strings.ToLower(spec.PXEMAC)) {
		return fmt.Errorf("invalid enrolled PXE MAC %q", spec.PXEMAC)
	}
	if _, err := ParseUUID(spec.ProductUUID); err != nil {
		return fmt.Errorf("invalid DMI productUUID: %w", err)
	}
	expectedNQN := api.NQNPrefix + spec.DeviceID
	expectedHostNQN := api.HostNQNPrefix + spec.DeviceID
	if spec.SubsystemNQN != expectedNQN {
		return fmt.Errorf("subsystemNQN %q must equal %q", spec.SubsystemNQN, expectedNQN)
	}
	if spec.HostNQN != expectedHostNQN {
		return fmt.Errorf("hostNQN %q must equal %q", spec.HostNQN, expectedHostNQN)
	}
	if spec.Port != 4420 {
		return fmt.Errorf("NVMe/TCP port must be 4420, got %d", spec.Port)
	}
	if spec.Address == "" {
		return fmt.Errorf("target address is required")
	}
	if len(spec.Namespaces) < 2 {
		return fmt.Errorf("at least system (NSID 1) and userdata (NSID 2) namespaces are required")
	}
	seenID := map[int]bool{}
	seenName := map[string]bool{}
	hasSystem, hasUserdata := false, false
	for _, ns := range spec.Namespaces {
		if ns.ID <= 0 || ns.ID > 32 {
			return fmt.Errorf("invalid namespace ID %d", ns.ID)
		}
		if !nsNamePattern.MatchString(ns.Name) {
			return fmt.Errorf("invalid namespace name %q", ns.Name)
		}
		if seenID[ns.ID] || seenName[ns.Name] {
			return fmt.Errorf("duplicate namespace ID %d or name %q", ns.ID, ns.Name)
		}
		seenID[ns.ID] = true
		seenName[ns.Name] = true
		if ns.Bytes < 16*1024*1024 {
			return fmt.Errorf("namespace %q size %d is below minimum 16 MiB", ns.Name, ns.Bytes)
		}
		if ns.ID == 1 && ns.Name == "system" {
			hasSystem = true
		}
		if ns.ID == 2 && ns.Name == "userdata" {
			hasUserdata = true
		}
	}
	if !hasSystem || !hasUserdata {
		return fmt.Errorf("NSID 1 must be 'system' and NSID 2 must be 'userdata'")
	}
	return nil
}

// Inspect reads live backing files, GPT partitions, and configfs nvmet state for a device.
func (m *Manager) Inspect(spec api.SANDeviceSpec) api.SANTargetStatus {
	nqn := spec.SubsystemNQN
	if nqn == "" {
		nqn = api.NQNPrefix + spec.DeviceID
	}
	st := api.SANTargetStatus{
		DeviceID:     spec.DeviceID,
		SubsystemNQN: nqn,
		Healthy:      true,
	}
	devRoot := filepath.Join(m.SANRoot, spec.DeviceID)
	st.NOCOWVerified = CheckNOCOW(devRoot)

	subsysPath := filepath.Join(m.ConfigfsRoot, "subsystems", configfsSegment(nqn))
	if info, err := os.Stat(subsysPath); err == nil && info.IsDir() {
		st.SubsystemExists = true
		st.Model = readTrimmed(filepath.Join(subsysPath, "attr_model"))
		st.AllowAnyHost = readTrimmed(filepath.Join(subsysPath, "attr_allow_any_host")) != "0"
		if st.AllowAnyHost {
			st.Healthy = false
			st.Issues = append(st.Issues, "attr_allow_any_host is not 0")
		}
		if entries, err := os.ReadDir(filepath.Join(subsysPath, "allowed_hosts")); err == nil {
			for _, e := range entries {
				st.AllowedHosts = append(st.AllowedHosts, fromConfigfsSegment(e.Name()))
			}
			sort.Strings(st.AllowedHosts)
		}
	} else {
		st.Healthy = false
		st.Issues = append(st.Issues, "nvmet subsystem not configured in configfs")
	}

	if spec.ConfigfsPort > 0 {
		portDir := filepath.Join(m.ConfigfsRoot, "ports", strconv.Itoa(spec.ConfigfsPort))
		if info, err := os.Stat(portDir); err == nil && info.IsDir() {
			st.ListenerAddress = readTrimmed(filepath.Join(portDir, "addr_traddr"))
			st.ListenerPort, _ = strconv.Atoi(readTrimmed(filepath.Join(portDir, "addr_trsvcid")))
			if _, err := os.Lstat(filepath.Join(portDir, "subsystems", configfsSegment(nqn))); err == nil {
				st.ListenerActive = true
			}
		}
	}

	for _, ns := range spec.Namespaces {
		rawFile := filepath.Join(devRoot, ns.Name+".raw")
		nsDir := filepath.Join(subsysPath, "namespaces", strconv.Itoa(ns.ID))
		expectedUUID := NamespaceUUID(nqn, ns.Name)
		row := api.SANNamespaceStatus{
			ID:           ns.ID,
			Name:         ns.Name,
			BackingFile:  rawFile,
			ExpectedWWID: "uuid." + expectedUUID,
			DeviceUUID:   readTrimmed(filepath.Join(nsDir, "device_uuid")),
			Enabled:      readTrimmed(filepath.Join(nsDir, "enable")) == "1",
		}
		if fi, err := os.Lstat(rawFile); err == nil && fi.Mode().IsRegular() {
			row.FileExists = true
			row.ActualBytes = fi.Size()
			if ns.ID == 1 && fi.Size() >= int64(6*SectorSize4K) {
				if f, oerr := os.Open(rawFile); oerr == nil {
					if parts, gerr := ReadGPT4K(f, fi.Size()); gerr == nil {
						row.GPTVerified = true
						for _, p := range parts {
							row.GPTPartitions = append(row.GPTPartitions, fmt.Sprintf("%s[%d..%d]", p.Name, p.StartLBA, p.EndLBA))
						}
					}
					f.Close()
				}
			}
		} else {
			st.Healthy = false
			st.Issues = append(st.Issues, fmt.Sprintf("backing file missing for namespace %s", ns.Name))
		}
		if !row.Enabled || (row.DeviceUUID != "" && row.DeviceUUID != expectedUUID) {
			st.Healthy = false
		}
		st.Namespaces = append(st.Namespaces, row)
	}
	return st
}

// Provision reconciles backing files, 4KiB system GPT, and nvmet configfs entries.
// When req.HotAddOnly is true, existing enabled namespaces and active listeners are left untouched.
func (m *Manager) Provision(req api.SANProvisionRequest) (*api.SANTargetStatus, error) {
	if err := ValidateDeviceSpec(req.Spec); err != nil {
		return nil, err
	}
	if m.RequireBtrfs {
		if err := VerifyBtrfsBacking(m.SANRoot); err != nil {
			return nil, err
		}
	}
	if req.DryRun {
		status := m.Inspect(req.Spec)
		return &status, nil
	}

	devRoot := filepath.Join(m.SANRoot, req.Spec.DeviceID)
	if lstat, err := os.Lstat(devRoot); err == nil {
		if lstat.Mode()&os.ModeSymlink != 0 || !lstat.IsDir() {
			return nil, fmt.Errorf("refusing non-directory or symlink SAN path %s", devRoot)
		}
	} else if os.IsNotExist(err) {
		if err := os.MkdirAll(devRoot, 0o700); err != nil {
			return nil, err
		}
	} else {
		return nil, err
	}
	if m.RequireBtrfs {
		if err := EnsureNOCOW(devRoot); err != nil {
			return nil, err
		}
	}

	nqn := req.Spec.SubsystemNQN
	hostNQN := req.Spec.HostNQN
	subsysDir := filepath.Join(m.ConfigfsRoot, "subsystems", configfsSegment(nqn))
	existed := dirExists(subsysDir)
	if err := os.MkdirAll(filepath.Join(subsysDir, "allowed_hosts"), 0o755); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(filepath.Join(subsysDir, "namespaces"), 0o755); err != nil {
		return nil, err
	}
	expectedModel := "PITS " + req.Spec.DeviceID
	if existed {
		curModel := readTrimmed(filepath.Join(subsysDir, "attr_model"))
		if curModel != "" && curModel != expectedModel {
			return nil, fmt.Errorf("refusing to adopt foreign subsystem %s with model %q", nqn, curModel)
		}
	} else {
		if err := os.WriteFile(filepath.Join(subsysDir, "attr_model"), []byte(expectedModel), 0o644); err != nil {
			return nil, fmt.Errorf("write attr_model: %w", err)
		}
	}
	if err := os.WriteFile(filepath.Join(subsysDir, "attr_allow_any_host"), []byte("0"), 0o644); err != nil {
		return nil, fmt.Errorf("write attr_allow_any_host: %w", err)
	}

	hostDir := filepath.Join(m.ConfigfsRoot, "hosts", configfsSegment(hostNQN))
	if err := os.MkdirAll(hostDir, 0o755); err != nil {
		return nil, err
	}
	if err := ensureSymlink(filepath.Join(subsysDir, "allowed_hosts", configfsSegment(hostNQN)), hostDir); err != nil {
		return nil, err
	}

	for _, ns := range req.Spec.Namespaces {
		aligned := Align4K(ns.Bytes)
		rawPath := filepath.Join(devRoot, ns.Name+".raw")
		createdNew := false
		var f *os.File
		if _, err := os.Lstat(rawPath); os.IsNotExist(err) {
			f, err = OpenNoFollow(rawPath, true)
			if err != nil {
				return nil, fmt.Errorf("create %s: %w", rawPath, err)
			}
			createdNew = true
		} else if err != nil {
			return nil, err
		} else {
			f, err = OpenNoFollow(rawPath, false)
			if err != nil {
				return nil, fmt.Errorf("open %s: %w", rawPath, err)
			}
		}

		st, err := f.Stat()
		if err != nil {
			f.Close()
			return nil, err
		}
		if st.Size() > aligned {
			f.Close()
			return nil, fmt.Errorf("shrinking SAN namespace %s (%d -> %d) is strictly forbidden", ns.Name, st.Size(), aligned)
		}
		if st.Size() < aligned {
			if err := PreallocateExtents(f, st.Size(), aligned-st.Size()); err != nil {
				f.Close()
				return nil, err
			}
		}
		if (createdNew && ns.ID == 1 && ns.Name == "system") || ns.InitSystemGPT {
			if parts, gerr := ReadGPT4K(f, aligned); gerr != nil || len(parts) == 0 {
				parts, werr := WriteSystemGPT4K(f, aligned)
				if werr != nil {
					f.Close()
					return nil, fmt.Errorf("initialize META+STATE GPT on %s: %w", rawPath, werr)
				}
				if err := f.Sync(); err != nil {
					f.Close()
					return nil, err
				}
				gptRecord, _ := json.MarshalIndent(parts, "", "  ")
				_ = os.WriteFile(filepath.Join(devRoot, "initial-gpt.json"), gptRecord, 0o600)
			}
		}
		f.Close()

		nsDir := filepath.Join(subsysDir, "namespaces", strconv.Itoa(ns.ID))
		if err := os.MkdirAll(nsDir, 0o755); err != nil {
			return nil, err
		}
		expectedUUID := NamespaceUUID(nqn, ns.Name)
		enabled := readTrimmed(filepath.Join(nsDir, "enable")) == "1"
		if enabled {
			if gotPath := readTrimmed(filepath.Join(nsDir, "device_path")); gotPath != rawPath {
				return nil, fmt.Errorf("enabled namespace %d points to %q instead of %q", ns.ID, gotPath, rawPath)
			}
			if gotUUID := readTrimmed(filepath.Join(nsDir, "device_uuid")); gotUUID != expectedUUID {
				return nil, fmt.Errorf("enabled namespace %d UUID %q mismatches contract %q", ns.ID, gotUUID, expectedUUID)
			}
			if !req.HotAddOnly {
				reval := filepath.Join(nsDir, "revalidate_size")
				if fileExists(reval) {
					_ = os.WriteFile(reval, []byte("1"), 0o644)
				}
			}
		} else {
			if err := os.WriteFile(filepath.Join(nsDir, "device_path"), []byte(rawPath), 0o644); err != nil {
				return nil, err
			}
			if err := os.WriteFile(filepath.Join(nsDir, "device_uuid"), []byte(expectedUUID), 0o644); err != nil {
				return nil, err
			}
			if err := os.WriteFile(filepath.Join(nsDir, "enable"), []byte("1"), 0o644); err != nil {
				return nil, err
			}
		}
	}

	if req.ConfigureListener && !req.HotAddOnly && req.Spec.ConfigfsPort > 0 {
		if err := m.reconcileListener(req.Spec); err != nil {
			return nil, err
		}
	}

	status := m.Inspect(req.Spec)
	allocBytes, _ := json.MarshalIndent(status, "", "  ")
	_ = writeAtomicFile(filepath.Join(devRoot, "allocation.json"), allocBytes, 0o600)
	return &status, nil
}

// Expand grows an existing namespace backing file additively and triggers configfs revalidate_size.
func (m *Manager) Expand(req api.SANExpandRequest) (*api.SANNamespaceStatus, error) {
	if !deviceIDPattern.MatchString(req.DeviceID) {
		return nil, fmt.Errorf("invalid deviceID %q", req.DeviceID)
	}
	aligned := Align4K(req.NewBytes)
	nqn := api.NQNPrefix + req.DeviceID
	nsDir := filepath.Join(m.ConfigfsRoot, "subsystems", configfsSegment(nqn), "namespaces", strconv.Itoa(req.NamespaceID))
	rawPath := readTrimmed(filepath.Join(nsDir, "device_path"))
	if rawPath == "" {
		return nil, fmt.Errorf("namespace %d on %s is not registered in configfs", req.NamespaceID, req.DeviceID)
	}
	f, err := OpenNoFollow(rawPath, false)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if aligned <= st.Size() {
		return nil, fmt.Errorf("expansion requires target size (%d) strictly greater than current size (%d)", aligned, st.Size())
	}
	if !req.DryRun {
		if err := PreallocateExtents(f, st.Size(), aligned-st.Size()); err != nil {
			return nil, err
		}
		reval := filepath.Join(nsDir, "revalidate_size")
		if fileExists(reval) {
			if err := os.WriteFile(reval, []byte("1"), 0o644); err != nil {
				return nil, err
			}
		}
	}
	base := strings.TrimSuffix(filepath.Base(rawPath), ".raw")
	return &api.SANNamespaceStatus{
		ID:           req.NamespaceID,
		Name:         base,
		BackingFile:  rawPath,
		FileExists:   true,
		ActualBytes:  aligned,
		DeviceUUID:   readTrimmed(filepath.Join(nsDir, "device_uuid")),
		ExpectedWWID: ExpectedWWID(nqn, base),
		Enabled:      readTrimmed(filepath.Join(nsDir, "enable")) == "1",
	}, nil
}

func (m *Manager) reconcileListener(spec api.SANDeviceSpec) error {
	portDir := filepath.Join(m.ConfigfsRoot, "ports", strconv.Itoa(spec.ConfigfsPort))
	if dirExists(portDir) {
		curAddr := readTrimmed(filepath.Join(portDir, "addr_traddr"))
		if curAddr != "" && curAddr != spec.Address {
			return fmt.Errorf("refusing to alter foreign configfs port %d bound to %s", spec.ConfigfsPort, curAddr)
		}
	}
	if err := os.MkdirAll(filepath.Join(portDir, "subsystems"), 0o755); err != nil {
		return err
	}
	for k, v := range map[string]string{
		"addr_trtype":  "tcp",
		"addr_adrfam":  "ipv4",
		"addr_traddr":  spec.Address,
		"addr_trsvcid": strconv.Itoa(spec.Port),
	} {
		if err := os.WriteFile(filepath.Join(portDir, k), []byte(v), 0o644); err != nil {
			return err
		}
	}
	subsysDir := filepath.Join(m.ConfigfsRoot, "subsystems", configfsSegment(spec.SubsystemNQN))
	return ensureSymlink(filepath.Join(portDir, "subsystems", configfsSegment(spec.SubsystemNQN)), subsysDir)
}

func ensureSymlink(linkPath, targetPath string) error {
	if fi, err := os.Lstat(linkPath); err == nil {
		if fi.Mode()&os.ModeSymlink == 0 {
			if runtime.GOOS == "windows" {
				if filepath.Clean(readTrimmed(linkPath)) == filepath.Clean(targetPath) {
					return nil
				}
			}
			return fmt.Errorf("unexpected non-symlink at %s", linkPath)
		}
		dest, rerr := os.Readlink(linkPath)
		if rerr != nil {
			return rerr
		}
		if filepath.Clean(dest) == filepath.Clean(targetPath) {
			return nil
		}
		return fmt.Errorf("conflicting configfs symlink %s -> %s", linkPath, dest)
	}
	if err := os.Symlink(targetPath, linkPath); err != nil {
		if runtime.GOOS == "windows" {
			return os.WriteFile(linkPath, []byte(targetPath), 0o644)
		}
		return err
	}
	return nil
}

func configfsSegment(name string) string {
	if runtime.GOOS == "windows" {
		return strings.ReplaceAll(name, ":", "_")
	}
	return name
}

func fromConfigfsSegment(seg string) string {
	if runtime.GOOS == "windows" && strings.HasPrefix(seg, "nqn.") {
		return strings.Replace(seg, "_", ":", 1)
	}
	return seg
}

func readTrimmed(path string) string {
	b, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(b))
}

func dirExists(path string) bool {
	st, err := os.Stat(path)
	return err == nil && st.IsDir()
}

func fileExists(path string) bool {
	st, err := os.Stat(path)
	return err == nil && !st.IsDir()
}

func writeAtomicFile(path string, data []byte, perm os.FileMode) error {
	tmp := path + ".new"
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, perm)
	if err != nil {
		return err
	}
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
