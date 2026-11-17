// Package san implements pure-Go NVMe/TCP target reconciliation, deterministic
// UUIDv5 WWID derivation, 4KiB-sector GPT (META + STATE) initialization, and
// additive-only Btrfs NOCOW backing image management.
package san

import (
	"crypto/rand"
	"crypto/sha1"
	"encoding/hex"
	"fmt"
	"regexp"
	"strings"
)

// NamespaceDNS is the RFC 4122 DNS namespace UUID (6ba7b810-9dad-11d1-80b4-00c04fd430c8).
var NamespaceDNS = [16]byte{
	0x6b, 0xa7, 0xb8, 0x10,
	0x9d, 0xad,
	0x11, 0xd1,
	0x80, 0xb4,
	0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
}

var uuidPattern = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)

// UUIDv5 computes an RFC 4122 version 5 SHA-1 UUID string in canonical lowercase form.
func UUIDv5(ns [16]byte, name string) string {
	h := sha1.New()
	h.Write(ns[:])
	h.Write([]byte(name))
	sum := h.Sum(nil)
	var u [16]byte
	copy(u[:], sum[:16])
	u[6] = (u[6] & 0x0f) | 0x50 // version 5
	u[8] = (u[8] & 0x3f) | 0x80 // RFC 4122 variant
	return formatUUID(u)
}

// RandomUUIDv4 generates a random RFC 4122 version 4 UUID.
func RandomUUIDv4() ([16]byte, error) {
	var u [16]byte
	if _, err := rand.Read(u[:]); err != nil {
		return u, err
	}
	u[6] = (u[6] & 0x0f) | 0x40
	u[8] = (u[8] & 0x3f) | 0x80
	return u, nil
}

// NamespaceUUID computes the deterministic NVMe namespace UUID used across PITS
// contracts: uuid5(NAMESPACE_DNS, subsystemNQN + "/" + namespaceName).
func NamespaceUUID(subsystemNQN, namespaceName string) string {
	return UUIDv5(NamespaceDNS, subsystemNQN+"/"+namespaceName)
}

// ExpectedWWID returns the Linux NVMe WWID string ("uuid.<namespaceUUID>").
func ExpectedWWID(subsystemNQN, namespaceName string) string {
	return "uuid." + NamespaceUUID(subsystemNQN, namespaceName)
}

// ParseUUID validates and parses a canonical UUID string into 16 bytes.
func ParseUUID(s string) ([16]byte, error) {
	var out [16]byte
	lower := strings.ToLower(strings.TrimSpace(s))
	if !uuidPattern.MatchString(lower) {
		return out, fmt.Errorf("invalid UUID %q", s)
	}
	clean := strings.ReplaceAll(lower, "-", "")
	b, err := hex.DecodeString(clean)
	if err != nil || len(b) != 16 {
		return out, fmt.Errorf("invalid UUID bytes %q", s)
	}
	copy(out[:], b)
	return out, nil
}

// Align4K rounds byte size upward to the 4096-byte NVMe/Btrfs sector boundary.
func Align4K(n int64) int64 {
	if n <= 0 {
		return 0
	}
	return ((n + 4095) / 4096) * 4096
}

func formatUUID(u [16]byte) string {
	return fmt.Sprintf("%08x-%04x-%04x-%04x-%012x",
		u[0:4], u[4:6], u[6:8], u[8:10], u[10:16])
}
