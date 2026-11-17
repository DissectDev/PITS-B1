package san

import (
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"io"
	"unicode/utf16"
)

const (
	SectorSize4K       = 4096
	GPTHeaderSize      = 92
	GPTEntryCount      = 128
	GPTEntrySize       = 128
	GPTEntrySectors4K  = (GPTEntryCount * GPTEntrySize) / SectorSize4K // 4 sectors
	FirstUsableLBA4K   = 256                                           // 1 MiB aligned at 4KiB sectors
	MetaStartLBA       = 256
	MetaSizeSectors    = 256    // 1 MiB
	StateStartLBA      = 512    // immediately after META
	StateSizeSectors   = 262144 // 1 GiB (262144 * 4096 = 1,073,741,824 bytes)
)

// LinuxFilesystemTypeGUID is 0FC63DAF-8483-4772-8E79-3D69D8477DE4 in mixed-endian GPT binary form.
var LinuxFilesystemTypeGUID = [16]byte{
	0xaf, 0x3d, 0xc6, 0x0f,
	0x83, 0x84,
	0x72, 0x47,
	0x8e, 0x79,
	0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4,
}

// GPTPartition summarizes one partition entry read from or written to a 4KiB GPT image.
type GPTPartition struct {
	Name     string `json:"name"`
	StartLBA uint64 `json:"startLba"`
	EndLBA   uint64 `json:"endLba"`
	Sectors  uint64 `json:"sectors"`
}

// WriteSystemGPT4K writes a complete Protective MBR + Primary/Backup 4096-byte-sector
// GPT partition table containing Talos META (1 MiB) and STATE (1 GiB) partitions,
// eliminating any dependency on loop devices, losetup, or sfdisk.
func WriteSystemGPT4K(w io.WriterAt, totalBytes int64) ([]GPTPartition, error) {
	if totalBytes%SectorSize4K != 0 {
		return nil, fmt.Errorf("image size %d is not 4096-byte aligned", totalBytes)
	}
	totalSectors := uint64(totalBytes / SectorSize4K)
	minSectors := uint64(StateStartLBA + StateSizeSectors + GPTEntrySectors4K + 32)
	if totalSectors < minSectors {
		return nil, fmt.Errorf("system image too small (%d sectors < %d required for META+STATE)", totalSectors, minSectors)
	}
	diskGUID, err := uuidToMixedEndianRandom()
	if err != nil {
		return nil, err
	}
	metaGUID, err := uuidToMixedEndianRandom()
	if err != nil {
		return nil, err
	}
	stateGUID, err := uuidToMixedEndianRandom()
	if err != nil {
		return nil, err
	}

	lastUsableLBA := totalSectors - 1 - GPTEntrySectors4K - 1
	backupEntriesLBA := totalSectors - 1 - GPTEntrySectors4K
	backupHeaderLBA := totalSectors - 1

	entriesBuf := make([]byte, GPTEntryCount*GPTEntrySize)
	encodeGPTEntry(entriesBuf[0:128], LinuxFilesystemTypeGUID, metaGUID, MetaStartLBA, MetaStartLBA+MetaSizeSectors-1, "META")
	encodeGPTEntry(entriesBuf[128:256], LinuxFilesystemTypeGUID, stateGUID, StateStartLBA, StateStartLBA+StateSizeSectors-1, "STATE")
	entriesCRC := crc32.ChecksumIEEE(entriesBuf)

	// 1. Protective MBR at LBA 0 (sector size 4096).
	mbr := make([]byte, SectorSize4K)
	// Partition record 1 at offset 446: type 0xEE covering LBA 1..min(totalSectors-1, 0xFFFFFFFF).
	mbr[446+0] = 0x00
	mbr[446+1] = 0x00
	mbr[446+2] = 0x02
	mbr[446+3] = 0x00
	mbr[446+4] = 0xEE
	mbr[446+5] = 0xFF
	mbr[446+6] = 0xFF
	mbr[446+7] = 0xFF
	binary.LittleEndian.PutUint32(mbr[446+8:446+12], 1)
	span := totalSectors - 1
	if span > 0xFFFFFFFF {
		span = 0xFFFFFFFF
	}
	binary.LittleEndian.PutUint32(mbr[446+12:446+16], uint32(span))
	mbr[510] = 0x55
	mbr[511] = 0xAA
	if _, err := w.WriteAt(mbr, 0); err != nil {
		return nil, fmt.Errorf("write protective MBR: %w", err)
	}

	// 2. Primary GPT Header at LBA 1 + Primary Entries at LBA 2..5.
	primaryHdr := buildGPTHeader(1, backupHeaderLBA, FirstUsableLBA4K, lastUsableLBA, diskGUID, 2, entriesCRC)
	if _, err := w.WriteAt(primaryHdr, SectorSize4K); err != nil {
		return nil, fmt.Errorf("write primary GPT header: %w", err)
	}
	if _, err := w.WriteAt(entriesBuf, 2*SectorSize4K); err != nil {
		return nil, fmt.Errorf("write primary GPT entries: %w", err)
	}

	// 3. Backup Entries + Backup GPT Header at end of disk.
	if _, err := w.WriteAt(entriesBuf, int64(backupEntriesLBA)*SectorSize4K); err != nil {
		return nil, fmt.Errorf("write backup GPT entries: %w", err)
	}
	backupHdr := buildGPTHeader(backupHeaderLBA, 1, FirstUsableLBA4K, lastUsableLBA, diskGUID, backupEntriesLBA, entriesCRC)
	if _, err := w.WriteAt(backupHdr, int64(backupHeaderLBA)*SectorSize4K); err != nil {
		return nil, fmt.Errorf("write backup GPT header: %w", err)
	}

	return []GPTPartition{
		{Name: "META", StartLBA: MetaStartLBA, EndLBA: MetaStartLBA + MetaSizeSectors - 1, Sectors: MetaSizeSectors},
		{Name: "STATE", StartLBA: StateStartLBA, EndLBA: StateStartLBA + StateSizeSectors - 1, Sectors: StateSizeSectors},
	}, nil
}

// ReadGPT4K reads and CRC-verifies the primary 4096-byte-sector GPT partition table.
func ReadGPT4K(r io.ReaderAt, totalBytes int64) ([]GPTPartition, error) {
	if totalBytes < int64((2+GPTEntrySectors4K)*SectorSize4K) {
		return nil, fmt.Errorf("file too small to contain 4K GPT")
	}
	hdr := make([]byte, SectorSize4K)
	if _, err := r.ReadAt(hdr, SectorSize4K); err != nil {
		return nil, err
	}
	if string(hdr[0:8]) != "EFI PART" {
		return nil, fmt.Errorf("missing EFI PART signature at 4KiB LBA 1")
	}
	hdrSize := binary.LittleEndian.Uint32(hdr[12:16])
	if hdrSize < GPTHeaderSize || int(hdrSize) > SectorSize4K {
		return nil, fmt.Errorf("invalid GPT header size %d", hdrSize)
	}
	wantHdrCRC := binary.LittleEndian.Uint32(hdr[16:20])
	hdrCopy := make([]byte, hdrSize)
	copy(hdrCopy, hdr[:hdrSize])
	binary.LittleEndian.PutUint32(hdrCopy[16:20], 0)
	if got := crc32.ChecksumIEEE(hdrCopy); got != wantHdrCRC {
		return nil, fmt.Errorf("GPT header CRC mismatch (got %08x, want %08x)", got, wantHdrCRC)
	}
	entriesLBA := binary.LittleEndian.Uint64(hdr[72:80])
	numEntries := binary.LittleEndian.Uint32(hdr[80:84])
	entrySize := binary.LittleEndian.Uint32(hdr[84:88])
	wantEntriesCRC := binary.LittleEndian.Uint32(hdr[88:92])
	if entrySize < 128 || numEntries == 0 || numEntries > 1024 {
		return nil, fmt.Errorf("unsupported GPT entry layout (%d x %d)", numEntries, entrySize)
	}
	entriesBuf := make([]byte, int(numEntries*entrySize))
	if _, err := r.ReadAt(entriesBuf, int64(entriesLBA)*SectorSize4K); err != nil {
		return nil, err
	}
	if got := crc32.ChecksumIEEE(entriesBuf); got != wantEntriesCRC {
		return nil, fmt.Errorf("GPT entries CRC mismatch")
	}
	var out []GPTPartition
	for i := uint32(0); i < numEntries; i++ {
		raw := entriesBuf[i*entrySize : (i+1)*entrySize]
		allZero := true
		for _, b := range raw[:16] {
			if b != 0 {
				allZero = false
				break
			}
		}
		if allZero {
			continue
		}
		start := binary.LittleEndian.Uint64(raw[32:40])
		end := binary.LittleEndian.Uint64(raw[40:48])
		name := decodeUTF16LEName(raw[56:128])
		var sectors uint64
		if end >= start {
			sectors = end - start + 1
		}
		out = append(out, GPTPartition{
			Name:     name,
			StartLBA: start,
			EndLBA:   end,
			Sectors:  sectors,
		})
	}
	return out, nil
}

func buildGPTHeader(myLBA, altLBA, firstUsable, lastUsable uint64, diskGUID [16]byte, entriesLBA uint64, entriesCRC uint32) []byte {
	sector := make([]byte, SectorSize4K)
	copy(sector[0:8], "EFI PART")
	binary.LittleEndian.PutUint32(sector[8:12], 0x00010000)
	binary.LittleEndian.PutUint32(sector[12:16], GPTHeaderSize)
	binary.LittleEndian.PutUint64(sector[24:32], myLBA)
	binary.LittleEndian.PutUint64(sector[32:40], altLBA)
	binary.LittleEndian.PutUint64(sector[40:48], firstUsable)
	binary.LittleEndian.PutUint64(sector[48:56], lastUsable)
	copy(sector[56:72], diskGUID[:])
	binary.LittleEndian.PutUint64(sector[72:80], entriesLBA)
	binary.LittleEndian.PutUint32(sector[80:84], GPTEntryCount)
	binary.LittleEndian.PutUint32(sector[84:88], GPTEntrySize)
	binary.LittleEndian.PutUint32(sector[88:92], entriesCRC)
	hdrCRC := crc32.ChecksumIEEE(sector[:GPTHeaderSize])
	binary.LittleEndian.PutUint32(sector[16:20], hdrCRC)
	return sector
}

func encodeGPTEntry(dst []byte, typeGUID, uniqueGUID [16]byte, startLBA, endLBA uint64, name string) {
	copy(dst[0:16], typeGUID[:])
	copy(dst[16:32], uniqueGUID[:])
	binary.LittleEndian.PutUint64(dst[32:40], startLBA)
	binary.LittleEndian.PutUint64(dst[40:48], endLBA)
	binary.LittleEndian.PutUint64(dst[48:56], 0)
	u16 := utf16.Encode([]rune(name))
	for i, code := range u16 {
		if 56+i*2+2 > 128 {
			break
		}
		binary.LittleEndian.PutUint16(dst[56+i*2:56+i*2+2], code)
	}
}

func decodeUTF16LEName(b []byte) string {
	var u16 []uint16
	for i := 0; i+1 < len(b); i += 2 {
		v := binary.LittleEndian.Uint16(b[i : i+2])
		if v == 0 {
			break
		}
		u16 = append(u16, v)
	}
	return string(utf16.Decode(u16))
}

func uuidToMixedEndianRandom() ([16]byte, error) {
	u, err := RandomUUIDv4()
	if err != nil {
		return u, err
	}
	// GPT encodes first 3 UUID fields little-endian.
	u[0], u[1], u[2], u[3] = u[3], u[2], u[1], u[0]
	u[4], u[5] = u[5], u[4]
	u[6], u[7] = u[7], u[6]
	return u, nil
}
