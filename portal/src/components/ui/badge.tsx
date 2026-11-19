import * as React from 'react';
import { cva, type VariantProps } from 'class-variance-authority';
import { cn } from '../../lib/utils';

const badgeVariants = cva(
  'inline-flex items-center gap-1 rounded-md border border-solid px-2 py-0.5 text-[11px] font-medium leading-tight transition-colors whitespace-nowrap',
  {
    variants: {
      variant: {
        default: 'border-transparent bg-primary text-primary-foreground',
        secondary: 'border-transparent bg-secondary text-secondary-foreground',
        outline: 'border-border bg-background text-foreground',
        muted: 'border-border bg-muted text-muted-foreground',
        ok: 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400',
        warn: 'border-amber-500/35 bg-amber-500/10 text-amber-700 dark:text-amber-400',
        error: 'border-red-500/35 bg-red-500/10 text-red-700 dark:text-red-400',
        info: 'border-sky-500/30 bg-sky-500/10 text-sky-700 dark:text-sky-400',
        purple: 'border-violet-500/30 bg-violet-500/10 text-violet-700 dark:text-violet-400',
        teal: 'border-teal-500/30 bg-teal-500/10 text-teal-700 dark:text-teal-400',
      },
    },
    defaultVariants: {
      variant: 'outline',
    },
  },
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

export const Badge = React.forwardRef<HTMLSpanElement, BadgeProps>(
  ({ className, variant, ...props }, ref) => (
    <span ref={ref} className={cn(badgeVariants({ variant }), className)} {...props} />
  ),
);
Badge.displayName = 'Badge';

export { badgeVariants };
