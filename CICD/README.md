# PITS Lab CI/CD & Crossplane Subproject (`CICD/`)

`CICD/` implements the `cluster-pits-lab` local CI/CD, caching, and Crossplane configuration plane designed in [docs/lab-cicd-crossplane-design.md](../docs/lab-cicd-crossplane-design.md), and takes over all subsequent build, verification, and deployment pipelines for `c:\Project\PITS\Infra`.

## Architecture Boundaries

- **Control/Data Isolation**: `Base/` (`pits-core` RKE2 Flux CD + NixOS hardware base) remains completely independent and untouched. Lab Flux CD (`flux-system` on `cluster-pits-lab`) watches only `CICD/` and `test-pool/manifests/`.
- **Hard `netboot-004` Exclusion**: Every Flux controller, Crossplane core/provider/function, CNPG instance, Gitea, Zot, Attic, Bootstrap Job, and `act_runner` deployment enforces `kubernetes.io/hostname NotIn [netboot-004]` so `win11-001` host resources are never contended.
- **Storage Tiering**:
  - `mayastor-2`: CNPG PostgreSQL (`2` instances, `20Gi`) + Gitea repository PVC (`50Gi`).
  - `openebs-single-replica`: Zot OCI Registry & Pull-Through Cache (`120Gi`) + Attic Nix Binary Cache (`120Gi`).
  - DDR5 `tmpfs` (`emptyDir: Memory`): `act_runner` execution pools (`runner-cicd-cpu`, `runner-cicd-npu`, `runner-cicd-gpu`).

## Endpoints (VLAN 20 Direct)

| Service | In-Cluster URL | Workstation URL | NodePort |
| :--- | :--- | :--- | :--- |
| Gitea | `http://gitea-http.pits-cicd.svc.cluster.local:3000` | `http://git.lab.pits.home.arpa:3000` | `30300` |
| Zot OCI Registry | `http://zot.pits-cicd.svc.cluster.local:5000` | `http://oci.lab.pits.home.arpa:5000` | `30500` |
| Attic Nix Cache | `http://attic.pits-cicd.svc.cluster.local:8080` | `http://nix-cache.lab.pits.home.arpa:8080` | `30808` |

## Directory Layout

- `bootstrap/` — Lab Flux CD v2.10.0 controllers (`gotk-components.yaml`) and scoped GitOps sync (`gotk-sync.yaml`).
- `crossplane/core/` — Crossplane v2.4.3 Helm values, `DeploymentRuntimeConfig`s, `Provider`s (`provider-helm`, `provider-kubernetes`), and `Function` (`function-go-templating`).
- `crossplane/xrds/` — `XLabDevStack` / `LabDevStack` XRD (`xlabdevstacks.lab.pits.io`).
- `crossplane/compositions/` — Go-templating Composition (`lab-devstack-composition`) orchestrating CNPG, Zot, Gitea, Attic, Bootstrap Job, and `act_runner` pools.
- `platform/` — Flux Kustomization root reconciling `crossplane/`, `iam/`, `cert-manager/`, `mesh/`, `gateway/`, and `instances/pits-lab-devstack.yaml` (`LabDevStack` Claim).
- `iam/` — Keycloak Crossplane declarations (`corp` Realm, `XAuthFlowBundle` FIDO2 flow, `pits-mode-a-gateway` Client).
- `cert-manager/` — `cert-manager v1.21.3` + Let's Encrypt Cloudflare DNS-01 `ClusterIssuer` (`*.lab.martensite.dev`).
- `mesh/` — Istio `1.30.8` Ambient control/data plane (`istio-base`, `istiod`, `istio-cni`, `ztunnel`) + Kubernetes Gateway API `v1.7.0` CRDs.
- `gateway/` — Zero-Trust north-south Istio Gateway (`lab-zero-trust-gateway`, `*.lab.martensite.dev`), `oauth2-proxy` (`ext_authz`), `HTTPRoute`s, `RequestAuthentication`, `AuthorizationPolicy` (`acr == "fido2"`), and `lab-split-dns` (`CoreDNS + Tailscale` subnet router).
- `image/` — Nix OCI image builder for `docker.io/pits/cicd-runner:v1.1` (`cicd-runner-image.nix`, `build-cicd-runner.ps1`) with Rust 1.95.0 + `x86_64-pc-windows-gnu` (MinGW-w64) cross-compilation.
- `scripts/` — Bootstrap, image mirroring, gateway secret provisioning, workstation access, verification, and core deployment handover scripts.

## Operations & Handover Workflows

1. **Bootstrap full Lab CI/CD stack**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Bootstrap-LabCicd.ps1
   ```
2. **Push & Run CI/CD Pipeline (`act_runner` on `cluster-pits-lab`)**:
   ```powershell
   # Case A: Uncommitted workspace edits (or local HEAD behind Lab Gitea snapshots) — pushes clean snapshot without altering local HEAD/index
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1 -Snapshot

   # Case B: First push after committing CICD/ locally to realign Lab Gitea main over prior test snapshots
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1 -Force

   # Case C: Standard committed workflow (add -DeployCore to hand off CD promotion after CI passes)
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Invoke-CicdPipeline.ps1
   ```
   > **Flux `prune: true` Safety Guard**: `Invoke-CicdPipeline.ps1` refuses to push a commit to `main` that lacks `CICD/platform/kustomization.yaml` (unless `-AllowMissingPlatform` is explicitly passed) so Lab Flux CD (`lab-cicd-platform`) never prunes `LabDevStack`.
3. **Build & Deploy Verified Artifacts (`portal`, `agent`, `test-runner`, `cicd-runner`, `windows`, `base`)**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Deploy-CoreArtifacts.ps1 -Target all
   ```
4. **Verify Health & `netboot-004` Exclusion**:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File c:\Project\PITS\Infra\CICD\scripts\Verify-CicdStack.ps1
   ```

## Workflow & Deployment Handover

- **Self-Hosted Gitea Actions Workflows (`.github/workflows/`)**:
  - `cicd.yml` (`pits-lab-rust`) — Validates all `CICD/` YAML/PowerShell/Bash artifacts, asserts `netboot-004` exclusion across every Deployment/Cluster/Job, and provides a `workflow_dispatch` CD hook referencing `CICD/scripts/Deploy-CoreArtifacts.ps1`.
  - `client.yml` (`pits-lab-rust` + `pits-lab-windows`), `server.yml` (`pits-lab-go`), `portal.yml` (`pits-lab-e2e`), `test-pool.yml` (`ubuntu-latest` + `pits-lab-npu`), and `base.yml` (`ubuntu-latest`) — Execute offline/DDR5 `tmpfs`-accelerated CI (including `x86_64-pc-windows-gnu` cross-compilation of `pitsctl.exe`) on `cluster-pits-lab` `act_runner` `:host` pods using `/opt/pits-cache` and `/ramdisk`.
- **Backward-Compatible Portal Promotion**: `deploy/nixos/promote-portal.ps1` remains the underlying NixOS promotion script invoked by `Deploy-CoreArtifacts.ps1 -Target portal` (or directly with `-ViaCicd` to route through the `CICD/` orchestrator).
