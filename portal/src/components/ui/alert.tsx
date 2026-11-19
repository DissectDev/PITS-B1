import * as React from 'react';
import { cva, type VariantProps } from 'class-variance-authority';
import { cn } from '../../lib/utils';

const alertVariants = cva(
  'pits-ui relative flex w-full items-start gap-2.5 rounded-lg border border-solid px-3.5 py-3 text-xs leading-relaxed [&>svg]:mt-0.5 [&>svg]:h-4 [&>svg]:w-4 [&>svg]:shrink-0',
  {
    variants: {
      variant: {
        default: 'border-border bg-muted/50 text-foreground [&>svg]:text-foreground',
        info: 'border-sky-500/30 bg-sky-500/[0.07] text-sky-900 dark:text-sky-200 [&>svg]:text-sky-600 dark:[&>svg]:text-sky-400',
        success:
          'border-emerald-500/30 bg-emerald-500/[0.07] text-emerald-900 dark:text-emerald-200 [&>svg]:text-emerald-600 dark:[&>svg]:text-emerald-400',
        warning:
          'border-amber-500/35 bg-amber-500/[0.08] text-amber-950 dark:text-amber-200 [&>svg]:text-amber-600 dark:[&>svg]:text-amber-400',
        destructive:
          'border-red-500/35 bg-red-500/[0.08] text-red-950 dark:text-red-200 [&>svg]:text-red-600 dark:[&>svg]:text-red-400',
      },
    },
    defaultVariants: {
      variant: 'default',
    },
  },
);

export interface AlertProps
  extends React.HTMLAttributes<HTMLDivElement>,
    VariantProps<typeof alertVariants> {}

export const Alert = React.forwardRef<HTMLDivElement, AlertProps>(
  ({ className, variant, ...props }, ref) => (
    <div
      ref={ref}
      role="alert"
      className={cn(alertVariants({ variant }), className)}
      {...props}
    />
  ),
);
Alert.displayName = 'Alert';

export const AlertTitle = React.forwardRef<
  HTMLParagraphElement,
  React.HTMLAttributes<HTMLHeadingElement>
>(({ className, ...props }, ref) => (
  <h5
    ref={ref}
    className={cn('m-0 mb-0.5 text-xs font-semibold leading-snug tracking-tight', className)}
    {...props}
  />
));
AlertTitle.displayName = 'AlertTitle';

export const AlertDescription = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div ref={ref} className={cn('text-xs leading-relaxed opacity-90', className)} {...props} />
));
AlertDescription.displayName = 'AlertDescription';
