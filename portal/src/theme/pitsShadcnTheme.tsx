import React, { useEffect, useState } from 'react';
import {
  createBaseThemeOptions,
  createUnifiedTheme,
  palettes,
  UnifiedThemeProvider,
  type PageTheme,
  type UnifiedTheme,
} from '@backstage/theme';
import { appThemeApiRef, useApi } from '@backstage/core-plugin-api';
import { PitsTooltipProvider } from '../components/ui/pits-primitives';

export type PitsThemeMode = 'system' | 'light' | 'dark';

function makeFlatPageTheme(bg: string, fg: string): PageTheme {
  return {
    colors: [bg, bg],
    shape: 'none',
    backgroundImage: 'none',
    fontColor: fg,
  };
}

function buildShadcnUnifiedTheme(variant: 'light' | 'dark'): UnifiedTheme {
  const isDark = variant === 'dark';
  const bg = isDark ? '#09090b' : '#ffffff';
  const card = isDark ? '#0e0e11' : '#ffffff';
  const muted = isDark ? '#18181b' : '#f4f4f5';
  const border = isDark ? '#27272a' : '#e4e4e7';
  const fg = isDark ? '#fafafa' : '#09090b';
  const fgMuted = isDark ? '#a1a1aa' : '#71717a';
  const primary = isDark ? '#fafafa' : '#18181b';
  const primaryContrast = isDark ? '#09090b' : '#fafafa';
  const linkColor = isDark ? '#e4e4e7' : '#18181b';

  const base = createBaseThemeOptions({
    palette: {
      ...(isDark ? palettes.dark : palettes.light),
      type: variant,
      mode: variant,
      background: {
        default: bg,
        paper: card,
      },
      primary: {
        main: primary,
        contrastText: primaryContrast,
      },
      secondary: {
        main: isDark ? '#a1a1aa' : '#52525b',
        contrastText: primaryContrast,
      },
      divider: border,
      border,
      text: {
        primary: fg,
        secondary: fgMuted,
      },
      textContrast: fg,
      textSubtle: fgMuted,
      textVerySubtle: isDark ? '#52525b' : '#a1a1aa',
      link: linkColor,
      linkHover: isDark ? '#ffffff' : '#09090b',
      navigation: {
        background: isDark ? '#0c0c0f' : '#fafafa',
        indicator: primary,
        color: fgMuted,
        selectedColor: fg,
        navItem: {
          hoverBackground: muted,
        },
      },
      tabbar: {
        indicator: primary,
      },
    },
  });

  const flatPage = makeFlatPageTheme(bg, fg);

  return createUnifiedTheme({
    ...base,
    fontFamily:
      'Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
    defaultPageTheme: 'home',
    pageTheme: {
      home: flatPage,
      documentation: flatPage,
      tool: flatPage,
      service: flatPage,
      website: flatPage,
      library: flatPage,
      other: flatPage,
      app: flatPage,
      apis: flatPage,
    },
    components: {
      MuiCssBaseline: {
        styleOverrides: {
          body: {
            backgroundColor: bg,
            color: fg,
          },
        },
      },
      MuiPaper: {
        defaultProps: {
          elevation: 0,
        },
        styleOverrides: {
          root: {
            backgroundImage: 'none',
            backgroundColor: card,
            borderColor: border,
          },
          outlined: {
            borderColor: border,
            borderRadius: 8,
          },
        },
      },
      MuiCard: {
        defaultProps: {
          elevation: 0,
        },
        styleOverrides: {
          root: {
            backgroundImage: 'none',
            backgroundColor: card,
            border: `1px solid ${border}`,
            borderRadius: 8,
            boxShadow: 'none',
          },
        },
      },
      MuiButton: {
        defaultProps: {
          disableElevation: true,
        },
        styleOverrides: {
          root: {
            textTransform: 'none',
            fontWeight: 500,
            borderRadius: 6,
            fontSize: '0.8125rem',
          },
          outlined: {
            borderColor: border,
          },
        },
      },
      MuiChip: {
        styleOverrides: {
          root: {
            borderRadius: 6,
            fontWeight: 500,
            fontSize: '0.75rem',
          },
          outlined: {
            borderColor: border,
          },
        },
      },
      MuiTableCell: {
        styleOverrides: {
          root: {
            borderBottomColor: border,
          },
          head: {
            fontWeight: 600,
            fontSize: '0.7rem',
            letterSpacing: '0.04em',
            textTransform: 'uppercase',
            color: fgMuted,
            backgroundColor: muted,
          },
        },
      },
      MuiOutlinedInput: {
        styleOverrides: {
          root: {
            borderRadius: 6,
            '& .MuiOutlinedInput-notchedOutline': {
              borderColor: border,
            },
          },
        },
      },
      MuiTab: {
        styleOverrides: {
          root: {
            textTransform: 'none',
            fontWeight: 500,
            minHeight: 42,
          },
        },
      },
      MuiDivider: {
        styleOverrides: {
          root: {
            borderColor: border,
          },
        },
      },
    },
  });
}

export const pitsLightTheme = buildShadcnUnifiedTheme('light');
export const pitsDarkTheme = buildShadcnUnifiedTheme('dark');

export const PitsShadcnThemeProvider: React.FC<{
  variant: 'light' | 'dark';
  children: React.ReactNode;
}> = ({ variant, children }) => {
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle('dark', variant === 'dark');
    root.setAttribute('data-theme-mode', variant);
    root.style.colorScheme = variant;
  }, [variant]);

  return (
    <UnifiedThemeProvider theme={variant === 'dark' ? pitsDarkTheme : pitsLightTheme}>
      <PitsTooltipProvider delayDuration={200}>{children}</PitsTooltipProvider>
    </UnifiedThemeProvider>
  );
};

function getOsPrefersDark(): boolean {
  if (typeof window === 'undefined' || !window.matchMedia) return false;
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

export function usePitsThemeMode(): {
  mode: PitsThemeMode;
  resolvedVariant: 'light' | 'dark';
  setMode: (next: PitsThemeMode) => void;
} {
  const appThemeApi = useApi(appThemeApiRef);
  const [activeId, setActiveId] = useState<string | undefined>(() =>
    appThemeApi.getActiveThemeId(),
  );
  const [osDark, setOsDark] = useState<boolean>(getOsPrefersDark);

  useEffect(() => {
    const sub = appThemeApi.activeThemeId$().subscribe(id => {
      setActiveId(id);
    });
    return () => sub.unsubscribe();
  }, [appThemeApi]);

  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return;
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = (e: MediaQueryListEvent) => setOsDark(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  const mode: PitsThemeMode =
    activeId === 'light' ? 'light' : activeId === 'dark' ? 'dark' : 'system';
  const resolvedVariant: 'light' | 'dark' =
    mode === 'system' ? (osDark ? 'dark' : 'light') : mode;

  const setMode = (next: PitsThemeMode) => {
    if (next === 'system') {
      appThemeApi.setActiveThemeId(undefined);
    } else {
      appThemeApi.setActiveThemeId(next);
    }
  };

  return { mode, resolvedVariant, setMode };
}
