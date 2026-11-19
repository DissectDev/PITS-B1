// pits-agent is the privileged Go control daemon running on pits-core (Linux).
// It replaces all host-level bash/python scripts for VFIO recovery, GoVPP
// inspection, Btrfs maintenance, NVMe/TCP configfs & 4KiB GPT provisioning,
// PXE asset verification, and allowlisted Core <-> Lab mTLS tunneling.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"pits.io/server/internal/api"
	"pits.io/server/internal/bridge"
	"pits.io/server/internal/btrfs"
	"pits.io/server/internal/clusters"
	"pits.io/server/internal/host"
	"pits.io/server/internal/kube"
	"pits.io/server/internal/pxe"
	"pits.io/server/internal/san"
	"pits.io/server/internal/vpp"
)

const version = "1.0.0"

func main() {
	if err := run(); err != nil {
		log.Printf("pits-agent fatal: %v", err)
		os.Exit(1)
	}
}

func run() error {
	listenAddr := flag.String("listen", "0.0.0.0:18443", "mTLS HTTPS listen address")
	caFile := flag.String("tls-ca", "/var/lib/pits/agent/pki/ca.crt", "Client CA PEM for mutual TLS")
	certFile := flag.String("tls-cert", "/var/lib/pits/agent/pki/server.crt", "Server TLS certificate PEM")
	keyFile := flag.String("tls-key", "/var/lib/pits/agent/pki/server.key", "Server TLS private key PEM")
	insecureHTTP := flag.Bool("insecure-loopback-http", false, "Serve plain HTTP (strictly for local loopback testing)")
	bearerToken := flag.String("bearer-token", "", "Optional additional Bearer token guard")
	kubeconfig := flag.String("kubeconfig", "/etc/rancher/rke2/rke2.yaml", "RKE2 kubeconfig path")
	btrfsMount := flag.String("btrfs-mount", api.DefaultBtrfsMount, "Btrfs mount directory")
	btrfsState := flag.String("btrfs-state", "/var/lib/pits/btrfs-manager", "Btrfs maintenance state directory")
	sanRoot := flag.String("san-root", api.DefaultSANRoot, "SAN backing files directory")
	configfsRoot := flag.String("configfs-nvmet", api.DefaultConfigfs, "Kernel nvmet configfs directory")
	vppSocket := flag.String("vpp-socket", api.DefaultVPPSocket, "GoVPP binary API socket (auto-falls back to /proc/<vpp>/root/run/vpp/api.sock)")
	pxeReports := flag.String("pxe-reports", "/var/lib/pits/tinkerbell-20261108/reports", "Hook hardware inventory JSON directory")
	pxeAssets := flag.String("pxe-assets", "/var/lib/pits/tinkerbell-20261108/public/talos", "Staged Talos netboot vmlinuz/initramfs directory")
	labNetns := flag.String("lab-netns", "auto", "Lab VLAN 20 netns path ('auto' dynamically discovers Multus Lab Pod under /proc)")
	clustersConfig := flag.String("clusters-config", "/var/lib/pits/agent/clusters.json", "Registered external Kubernetes clusters JSON registry")
	printSystemd := flag.Bool("print-systemd-unit", false, "Print hardened systemd service unit and exit")
	flag.Parse()

	if *printSystemd {
		fmt.Print(systemdUnitTemplate)
		return nil
	}

	tun := bridge.NewTunnel(*labNetns)
	clusterMgr := clusters.NewManager(*kubeconfig, *clustersConfig, nil)
	tun.ExtraTargetChecker = clusterMgr.AllowedTunnelTarget
	handler := api.NewHandler(&api.Handler{
		Version:     version,
		BearerToken: *bearerToken,
		Host:        host.NewController("/sys", "/proc", *kubeconfig),
		Btrfs:       btrfs.NewManager(*btrfsMount, "/sys", *btrfsState),
		SAN:         san.NewManager(*sanRoot, *configfsRoot, !*insecureHTTP),
		VPP:         vpp.NewInspector(*vppSocket),
		PXE:         pxe.NewStore(*pxeReports, *pxeAssets),
		Bridge:      tun,
		Routing:     tun,
		Clusters:    clusterMgr,
	})

	srv := &http.Server{
		Addr:              *listenAddr,
		Handler:           handler,
		ReadHeaderTimeout: 10 * time.Second,
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if kc, kerr := kube.NewFromKubeconfig(*kubeconfig); kerr != nil {
		log.Printf("pits-agent: EdgeRouter routing-table watch disabled (kubeconfig %s: %v)", *kubeconfig, kerr)
	} else {
		tun.StartSubscriber(ctx, kc)
	}

	errCh := make(chan error, 1)
	go func() {
		if *insecureHTTP {
			log.Printf("pits-agent %s listening (plain HTTP test mode) on %s", version, *listenAddr)
			errCh <- srv.ListenAndServe()
			return
		}
		tlsCfg, err := buildMutualTLS(*caFile, *certFile, *keyFile)
		if err != nil {
			errCh <- err
			return
		}
		srv.TLSConfig = tlsCfg
		log.Printf("pits-agent %s listening with mTLS (TLS 1.3) on %s", version, *listenAddr)
		errCh <- srv.ListenAndServeTLS("", "")
	}()

	select {
	case err := <-errCh:
		if err != nil && err != http.ErrServerClosed {
			return err
		}
		return nil
	case <-ctx.Done():
		shutCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		return srv.Shutdown(shutCtx)
	}
}

func buildMutualTLS(caPath, certPath, keyPath string) (*tls.Config, error) {
	caPEM, err := os.ReadFile(caPath)
	if err != nil {
		return nil, fmt.Errorf("read client CA %s: %w", caPath, err)
	}
	clientPool := x509.NewCertPool()
	if !clientPool.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("invalid PEM in client CA %s", caPath)
	}
	pair, err := tls.LoadX509KeyPair(certPath, keyPath)
	if err != nil {
		return nil, fmt.Errorf("load server keypair: %w", err)
	}
	return &tls.Config{
		MinVersion:   tls.VersionTLS13,
		ClientAuth:   tls.RequireAndVerifyClientCert,
		ClientCAs:    clientPool,
		Certificates: []tls.Certificate{pair},
	}, nil
}

const systemdUnitTemplate = `[Unit]
Description=PITS Privileged Host & SAN Control Daemon (Go)
After=network-online.target rke2-server.service
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/var/lib/pits/bin/pits-agent \
  -listen 0.0.0.0:18443 \
  -tls-ca /etc/pits-agent/pki/ca.crt \
  -tls-cert /etc/pits-agent/pki/server.crt \
  -tls-key /etc/pits-agent/pki/server.key \
  -kubeconfig /etc/rancher/rke2/rke2.yaml
Restart=on-failure
RestartSec=3s
UMask=0077

[Install]
WantedBy=multi-user.target
`
