import * as React from 'react';
import { Link as RouterLink } from 'react-router-dom';
import { ChevronRight } from 'lucide-react';
import { cn } from '../../lib/utils';

export interface PageMetaItem {
  label: string;
  value: string;
}

export const PageShell: React.FC<{
  children: React.ReactNode;
  className?: string;
}> = ({ children, className }) => (
  <div className={cn('pits-ui pits-shadcn-root min-h-screen bg-background text-foreground', className)}>
    {children}
  </div>
);

export const PageHeader: React.FC<{
  title: React.ReactNode;
  description?: React.ReactNode;
  breadcrumb?: { label: string; to: string };
  badge?: React.ReactNode;
  meta?: PageMetaItem[];
  actions?: React.ReactNode;
}> = ({ title, description, breadcrumb, badge, meta, actions }) => (
  <header className="border-b border-solid border-border bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80">
    <div className="mx-auto flex max-w-[1460px] flex-col gap-3 px-6 py-5 md:flex-row md:items-end md:justify-between">
      <div className="min-w-0 space-y-1.5">
        {breadcrumb && (
          <div className="flex items-center gap-1 text-xs text-muted-foreground">
            <RouterLink
              to={breadcrumb.to}
              className="text-muted-foreground transition-colors hover:text-foreground"
            >
              {breadcrumb.label}
            </RouterLink>
            <ChevronRight className="h-3.5 w-3.5 opacity-60" />
            <span className="text-foreground/80">Detail</span>
          </div>
        )}
        <div className="flex flex-wrap items-center gap-2.5">
          <h1 className="m-0 text-xl font-semibold tracking-tight text-foreground md:text-2xl">
            {title}
          </h1>
          {badge}
        </div>
        {description && (
          <p className="m-0 max-w-3xl text-[13px] leading-relaxed text-muted-foreground">
            {description}
          </p>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-4 md:justify-end">
        {meta && meta.length > 0 && (
          <div className="flex flex-wrap items-center gap-4">
            {meta.map((item, idx) => (
              <div
                key={item.label}
                className={cn(
                  'flex flex-col',
                  idx > 0 && 'border-l border-solid border-border pl-4',
                )}
              >
                <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
                  {item.label}
                </span>
                <span className="text-xs font-semibold text-foreground">{item.value}</span>
              </div>
            ))}
          </div>
        )}
        {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      </div>
    </div>
  </header>
);

export const PageBody: React.FC<{
  children: React.ReactNode;
  className?: string;
}> = ({ children, className }) => (
  <main className={cn('mx-auto max-w-[1460px] px-6 py-6', className)}>{children}</main>
);
