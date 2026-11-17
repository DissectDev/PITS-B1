import React, { useEffect } from 'react';
import { Box, CircularProgress, Stack, Typography } from '@mui/material';
import AppsIcon from '@mui/icons-material/Apps';
import CategoryOutlinedIcon from '@mui/icons-material/CategoryOutlined';
import AccountTreeOutlinedIcon from '@mui/icons-material/AccountTreeOutlined';
import DnsOutlinedIcon from '@mui/icons-material/DnsOutlined';
import MemoryOutlinedIcon from '@mui/icons-material/MemoryOutlined';
import DesktopWindowsOutlinedIcon from '@mui/icons-material/DesktopWindowsOutlined';
import RouterOutlinedIcon from '@mui/icons-material/RouterOutlined';
import SystemUpdateAltOutlinedIcon from '@mui/icons-material/SystemUpdateAltOutlined';
import StorageOutlinedIcon from '@mui/icons-material/StorageOutlined';
import ViewInArOutlinedIcon from '@mui/icons-material/ViewInArOutlined';
import HubOutlinedIcon from '@mui/icons-material/HubOutlined';
import TuneOutlinedIcon from '@mui/icons-material/TuneOutlined';
import AssessmentOutlinedIcon from '@mui/icons-material/AssessmentOutlined';
import CloudOutlinedIcon from '@mui/icons-material/CloudOutlined';
import CodeOutlinedIcon from '@mui/icons-material/CodeOutlined';
import DomainOutlinedIcon from '@mui/icons-material/DomainOutlined';
import GroupsOutlinedIcon from '@mui/icons-material/GroupsOutlined';
import LayersOutlinedIcon from '@mui/icons-material/LayersOutlined';
import WidgetsOutlinedIcon from '@mui/icons-material/WidgetsOutlined';
import {
  Sidebar,
  SidebarDivider,
  SidebarGroup,
  SidebarItem,
  SidebarScrollWrapper,
  SidebarSpace,
} from '@backstage/core-components';
import type { IdentityApi } from '@backstage/frontend-plugin-api';
import type {
  NavContentComponent,
  SignInPageProps,
} from '@backstage/plugin-app-react';

const DomainSectionLabel: React.FC<{ label: string }> = ({ label }) => (
  <Box sx={{ px: 2.5, pt: 1.25, pb: 0.4 }}>
    <Typography
      variant="caption"
      sx={{
        fontSize: '0.66rem',
        fontWeight: 700,
        letterSpacing: 0.8,
        textTransform: 'uppercase',
        color: 'rgba(255,255,255,0.52)',
        display: 'block',
        whiteSpace: 'nowrap',
        overflow: 'hidden',
        textOverflow: 'ellipsis',
      }}
    >
      {label}
    </Typography>
  </Box>
);

export const PitsSidebarNav: NavContentComponent = () => {
  return (
    <Sidebar>
      <Box
        sx={{
          px: 2.5,
          py: 2,
          display: 'flex',
          alignItems: 'center',
          gap: 1.25,
        }}
      >
        <Box
          sx={{
            width: 30,
            height: 30,
            borderRadius: 1.25,
            bgcolor: '#2563eb',
            color: '#ffffff',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            fontWeight: 900,
            fontSize: '0.88rem',
            flexShrink: 0,
            boxShadow: '0 2px 8px rgba(37, 99, 235, 0.45)',
          }}
        >
          P
        </Box>
        <Box sx={{ minWidth: 0 }}>
          <Typography
            variant="subtitle2"
            sx={{
              color: '#ffffff',
              fontWeight: 800,
              lineHeight: 1.15,
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
            }}
          >
            PITS Portal
          </Typography>
          <Typography
            variant="caption"
            sx={{
              color: 'rgba(255,255,255,0.62)',
              fontSize: '0.68rem',
              display: 'block',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
            }}
          >
            Entity Registry · Backstage v1.56
          </Typography>
        </Box>
      </Box>

      <SidebarDivider />

      <SidebarGroup label="Registry" icon={<AppsIcon />}>
        <SidebarItem
          icon={AppsIcon}
          to="/resource-types"
          text="Resource Directory"
        />
        <SidebarItem
          icon={CategoryOutlinedIcon}
          to="/catalog"
          text="Entity Registry"
        />
        <SidebarItem
          icon={AccountTreeOutlinedIcon}
          to="/catalog-graph"
          text="Topology Graph"
        />
      </SidebarGroup>

      <SidebarDivider />

      <SidebarScrollWrapper>
        <SidebarGroup label="Architecture & Resources" icon={<DnsOutlinedIcon />}>
          <DomainSectionLabel label="Architecture (4-Layer Model)" />
          <SidebarItem
            icon={DomainOutlinedIcon}
            to="/catalog?filters%5Bkind%5D=domain"
            text="Domains"
          />
          <SidebarItem
            icon={LayersOutlinedIcon}
            to="/catalog?filters%5Bkind%5D=system"
            text="Systems"
          />
          <SidebarItem
            icon={WidgetsOutlinedIcon}
            to="/catalog?filters%5Bkind%5D=component"
            text="Components"
          />

          <DomainSectionLabel label="Infrastructure · Compute" />
          <SidebarItem
            icon={DnsOutlinedIcon}
            to="/resource-types/baremetal-host"
            text="Baremetal Hosts"
          />
          <SidebarItem
            icon={MemoryOutlinedIcon}
            to="/resource-types/npu-accelerator"
            text="AI NPU Accelerators"
          />
          <SidebarItem
            icon={DesktopWindowsOutlinedIcon}
            to="/resource-types/virtual-machine"
            text="Virtual Machines"
          />

          <DomainSectionLabel label="Infrastructure · Network & Boot" />
          <SidebarItem
            icon={RouterOutlinedIcon}
            to="/resource-types/edge-router"
            text="Edge Routers"
          />
          <SidebarItem
            icon={SystemUpdateAltOutlinedIcon}
            to="/resource-types/pxe-boot-profile"
            text="PXE Boot Profiles"
          />

          <DomainSectionLabel label="Infrastructure · Storage" />
          <SidebarItem
            icon={StorageOutlinedIcon}
            to="/resource-types/storage-pool"
            text="Storage Pools"
          />
          <SidebarItem
            icon={ViewInArOutlinedIcon}
            to="/resource-types/san-volume"
            text="SAN Volumes"
          />

          <DomainSectionLabel label="Infrastructure · Control" />
          <SidebarItem
            icon={HubOutlinedIcon}
            to="/resource-types/kubernetes-cluster"
            text="K8s Clusters"
          />

          <DomainSectionLabel label="Sourcing Governance" />
          <SidebarItem
            icon={AssessmentOutlinedIcon}
            to="/sourcing-audit"
            text="Sourcing Matrix & Sync"
          />
          <SidebarItem
            icon={CloudOutlinedIcon}
            to="/resource-types/vendor-saas"
            text="Vendor SaaS"
          />
          <SidebarItem
            icon={CodeOutlinedIcon}
            to="/resource-types/commercial-sdk"
            text="Commercial SDKs"
          />
          <SidebarItem
            icon={RouterOutlinedIcon}
            to="/resource-types/managed-infra"
            text="Managed Infra"
          />
          <SidebarItem
            icon={GroupsOutlinedIcon}
            to="/resource-types/outsourced-dev"
            text="Outsourced Dev"
          />
        </SidebarGroup>
      </SidebarScrollWrapper>

      <SidebarSpace />
      <SidebarDivider />

      <SidebarGroup label="Settings" icon={<TuneOutlinedIcon />}>
        <SidebarItem
          icon={TuneOutlinedIcon}
          to="/settings/pits-profile"
          text="Operator & mTLS"
        />
      </SidebarGroup>
    </Sidebar>
  );
};

interface GuestSessionPayload {
  profile?: {
    displayName?: string;
    email?: string;
  };
  backstageIdentity?: {
    token?: string;
    identity?: {
      type?: 'user';
      userEntityRef?: string;
      ownershipEntityRefs?: string[];
    };
  };
  token?: string;
  identity?: {
    type?: 'user';
    userEntityRef?: string;
    ownershipEntityRefs?: string[];
  };
}

async function fetchGuestSession(): Promise<{
  token?: string;
  displayName: string;
  email: string;
  userEntityRef: string;
  ownershipEntityRefs: string[];
}> {
  const origin =
    typeof window !== 'undefined' && window.location?.origin
      ? window.location.origin.replace(/\/$/, '')
      : '';
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 2500);
  try {
    const res = await fetch(`${origin}/api/auth/guest/refresh`, {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({}),
      signal: controller.signal,
    });
    if (res.ok) {
      const data = (await res.json()) as GuestSessionPayload;
      const token = data.backstageIdentity?.token || data.token;
      const idObj = data.backstageIdentity?.identity || data.identity;
      return {
        token,
        displayName: data.profile?.displayName || 'PITS Operator',
        email: data.profile?.email || 'pitsadmin@pits.home.arpa',
        userEntityRef: idObj?.userEntityRef || 'user:default/pitsadmin',
        ownershipEntityRefs: idObj?.ownershipEntityRefs || [
          'user:default/pitsadmin',
          'group:default/operators',
          'group:default/risk-engineering',
        ],
      };
    }
  } catch {
    // Fallback to local guest identity when running standalone preview
  } finally {
    clearTimeout(timeoutId);
  }
  return {
    token: undefined,
    displayName: 'PITS Operator',
    email: 'pitsadmin@pits.home.arpa',
    userEntityRef: 'user:default/pitsadmin',
    ownershipEntityRefs: [
      'user:default/pitsadmin',
      'group:default/operators',
      'group:default/risk-engineering',
    ],
  };
}

export const AutoGuestSignInPage: React.FC<SignInPageProps> = ({ onSignInSuccess }) => {
  useEffect(() => {
    let cancelled = false;

    void (async () => {
      const session = await fetchGuestSession();
      if (cancelled) return;
      let cachedToken = session.token;

      const identityApi: IdentityApi & {
        getUserId: () => string;
        getProfile: () => { displayName: string; email: string };
        getIdToken: () => Promise<string | undefined>;
      } = {
        getUserId: () =>
          session.userEntityRef.split('/').pop() || 'pitsadmin',
        getProfile: () => ({
          displayName: session.displayName,
          email: session.email,
        }),
        getIdToken: async () => cachedToken,
        getProfileInfo: async () => ({
          displayName: session.displayName,
          email: session.email,
        }),
        getBackstageIdentity: async () => ({
          type: 'user',
          userEntityRef: session.userEntityRef,
          ownershipEntityRefs: session.ownershipEntityRefs,
        }),
        getCredentials: async () => {
          if (!cachedToken) {
            const refreshed = await fetchGuestSession();
            cachedToken = refreshed.token;
          }
          return cachedToken ? { token: cachedToken } : {};
        },
        signOut: async () => {
          cachedToken = undefined;
        },
      };

      onSignInSuccess(identityApi);
    })();

    return () => {
      cancelled = true;
    };
  }, [onSignInSuccess]);

  return (
    <Stack
      alignItems="center"
      justifyContent="center"
      spacing={2}
      sx={{ minHeight: '100vh', bgcolor: 'background.default' }}
    >
      <CircularProgress size={30} />
      <Typography variant="body2" color="text.secondary">
        Initializing PITS Operator Guest Session...
      </Typography>
    </Stack>
  );
};
