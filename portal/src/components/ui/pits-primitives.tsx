import React from 'react';
import * as RadixDialog from '@radix-ui/react-dialog';
import * as RadixTabs from '@radix-ui/react-tabs';
import * as RadixSelect from '@radix-ui/react-select';
import * as RadixSwitch from '@radix-ui/react-switch';
import * as RadixCheckbox from '@radix-ui/react-checkbox';
import * as RadixSlider from '@radix-ui/react-slider';
import * as RadixTooltip from '@radix-ui/react-tooltip';
import * as RadixProgress from '@radix-ui/react-progress';
import { Check, ChevronDown, X } from 'lucide-react';
import { cn } from '../../lib/utils';

/* ───────────────────────── Dialog ───────────────────────── */

export const PitsDialog: React.FC<{
  open: boolean;
  onOpenChange: (next: boolean) => void;
  title: React.ReactNode;
  subtitle?: React.ReactNode;
  footer?: React.ReactNode;
  disableClose?: boolean;
  maxWidthClass?: string;
  children: React.ReactNode;
}> = ({
  open,
  onOpenChange,
  title,
  subtitle,
  footer,
  disableClose,
  maxWidthClass = 'max-w-3xl',
  children,
}) => (
  <RadixDialog.Root
    open={open}
    onOpenChange={next => {
      if (!next && disableClose) return;
      onOpenChange(next);
    }}
  >
    <RadixDialog.Portal>
      <RadixDialog.Overlay className="pits-ui fixed inset-0 z-50 bg-black/50 backdrop-blur-[2px]" />
      <RadixDialog.Content
        onEscapeKeyDown={e => {
          if (disableClose) e.preventDefault();
        }}
        onPointerDownOutside={e => {
          if (disableClose) e.preventDefault();
        }}
        className={cn(
          'pits-ui fixed left-1/2 top-1/2 z-50 flex max-h-[88vh] w-[92vw] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-xl border border-solid border-border bg-card text-card-foreground shadow-2xl focus:outline-none',
          maxWidthClass,
        )}
      >
        <div className="flex items-start justify-between gap-4 border-b border-solid border-border px-6 py-4">
          <div className="min-w-0 space-y-1">
            <RadixDialog.Title className="m-0 text-base font-semibold tracking-tight text-foreground">
              {title}
            </RadixDialog.Title>
            {subtitle && (
              <RadixDialog.Description className="m-0 text-xs text-muted-foreground">
                {subtitle}
              </RadixDialog.Description>
            )}
          </div>
          <button
            type="button"
            disabled={disableClose}
            onClick={() => onOpenChange(false)}
            aria-label="Close"
            className="inline-flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md border border-solid border-transparent bg-transparent text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="pits-scroll flex-1 overflow-y-auto px-6 py-5">{children}</div>

        {footer && (
          <div className="flex flex-wrap items-center justify-end gap-2 border-t border-solid border-border bg-muted/30 px-6 py-3.5">
            {footer}
          </div>
        )}
      </RadixDialog.Content>
    </RadixDialog.Portal>
  </RadixDialog.Root>
);

/* ───────────────────────── Tabs ───────────────────────── */

export interface PitsTabItem {
  value: string;
  label: React.ReactNode;
  icon?: React.ReactNode;
}

export const PitsTabs: React.FC<{
  value: string;
  onValueChange: (val: string) => void;
  items: PitsTabItem[];
  children: React.ReactNode;
  className?: string;
}> = ({ value, onValueChange, items, children, className }) => (
  <RadixTabs.Root value={value} onValueChange={onValueChange} className={cn('pits-ui', className)}>
    <div className="border-b border-solid border-border px-5 pt-3">
      <RadixTabs.List className="flex flex-wrap items-center gap-1">
        {items.map(item => (
          <RadixTabs.Trigger
            key={item.value}
            value={item.value}
            className="inline-flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-t-md border-0 border-b-2 border-solid border-transparent bg-transparent px-3.5 py-2.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground data-[state=active]:border-primary data-[state=active]:text-foreground"
          >
            {item.icon}
            <span>{item.label}</span>
          </RadixTabs.Trigger>
        ))}
      </RadixTabs.List>
    </div>
    {children}
  </RadixTabs.Root>
);

export const PitsTabPanel: React.FC<{
  value: string;
  className?: string;
  children: React.ReactNode;
}> = ({ value, className, children }) => (
  <RadixTabs.Content value={value} className={cn('p-5 focus:outline-none', className)}>
    {children}
  </RadixTabs.Content>
);

/* ───────────────────────── Select ───────────────────────── */

export interface PitsSelectOption {
  value: string;
  label: string;
}

export const PitsSelect: React.FC<{
  value: string;
  onValueChange: (val: string) => void;
  options: PitsSelectOption[];
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  size?: 'sm' | 'default';
}> = ({ value, onValueChange, options, placeholder, disabled, className, size = 'default' }) => (
  <RadixSelect.Root value={value} onValueChange={onValueChange} disabled={disabled}>
    <RadixSelect.Trigger
      className={cn(
        'pits-ui inline-flex w-full cursor-pointer items-center justify-between gap-2 rounded-md border border-solid border-input bg-background px-3 text-left text-foreground shadow-sm transition-colors hover:bg-accent/40 focus:outline-none focus:ring-2 focus:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
        size === 'sm' ? 'h-8 text-xs' : 'h-9 text-[13px]',
        className,
      )}
    >
      <RadixSelect.Value placeholder={placeholder} />
      <RadixSelect.Icon asChild>
        <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      </RadixSelect.Icon>
    </RadixSelect.Trigger>
    <RadixSelect.Portal>
      <RadixSelect.Content
        position="popper"
        sideOffset={4}
        className="pits-ui z-[60] max-h-72 min-w-[var(--radix-select-trigger-width)] overflow-hidden rounded-lg border border-solid border-border bg-popover p-1 text-popover-foreground shadow-lg"
      >
        <RadixSelect.Viewport className="pits-scroll max-h-64 overflow-y-auto">
          {options.map(opt => (
            <RadixSelect.Item
              key={opt.value}
              value={opt.value}
              className="relative flex cursor-pointer select-none items-center rounded-md py-1.5 pl-7 pr-3 text-xs text-popover-foreground outline-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground"
            >
              <span className="absolute left-2 inline-flex h-3.5 w-3.5 items-center justify-center">
                <RadixSelect.ItemIndicator>
                  <Check className="h-3.5 w-3.5" />
                </RadixSelect.ItemIndicator>
              </span>
              <RadixSelect.ItemText>{opt.label}</RadixSelect.ItemText>
            </RadixSelect.Item>
          ))}
        </RadixSelect.Viewport>
      </RadixSelect.Content>
    </RadixSelect.Portal>
  </RadixSelect.Root>
);

/* ───────────────────────── Switch ───────────────────────── */

export const PitsSwitch: React.FC<{
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  tone?: 'default' | 'warning';
  id?: string;
}> = ({ checked, onCheckedChange, disabled, tone = 'default', id }) => (
  <RadixSwitch.Root
    id={id}
    checked={checked}
    onCheckedChange={onCheckedChange}
    disabled={disabled}
    className={cn(
      'pits-ui relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border-2 border-solid border-transparent p-0 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50',
      checked
        ? tone === 'warning'
          ? 'bg-amber-500'
          : 'bg-primary'
        : 'bg-input',
    )}
  >
    <RadixSwitch.Thumb
      className={cn(
        'pointer-events-none block h-4 w-4 rounded-full bg-background shadow-sm transition-transform',
        checked ? 'translate-x-4' : 'translate-x-0',
      )}
    />
  </RadixSwitch.Root>
);

/* ───────────────────────── Checkbox ───────────────────────── */

export const PitsCheckbox: React.FC<{
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  id?: string;
}> = ({ checked, onCheckedChange, disabled, id }) => (
  <RadixCheckbox.Root
    id={id}
    checked={checked}
    onCheckedChange={state => onCheckedChange(state === true)}
    disabled={disabled}
    className={cn(
      'pits-ui inline-flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded border border-solid border-input bg-background p-0 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 data-[state=checked]:border-primary data-[state=checked]:bg-primary data-[state=checked]:text-primary-foreground',
    )}
  >
    <RadixCheckbox.Indicator className="flex items-center justify-center">
      <Check className="h-3 w-3 stroke-[2.5]" />
    </RadixCheckbox.Indicator>
  </RadixCheckbox.Root>
);

/* ───────────────────────── Slider ───────────────────────── */

export const PitsSlider: React.FC<{
  value: number;
  onValueChange: (val: number) => void;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
}> = ({ value, onValueChange, min = 1, max = 5, step = 0.5, disabled }) => (
  <RadixSlider.Root
    value={[value]}
    onValueChange={vals => {
      if (typeof vals[0] === 'number') onValueChange(vals[0]);
    }}
    min={min}
    max={max}
    step={step}
    disabled={disabled}
    className="pits-ui relative flex h-5 w-full touch-none select-none items-center"
  >
    <RadixSlider.Track className="relative h-1.5 w-full grow overflow-hidden rounded-full bg-secondary">
      <RadixSlider.Range className="absolute h-full bg-primary" />
    </RadixSlider.Track>
    <RadixSlider.Thumb className="block h-4 w-4 cursor-pointer rounded-full border-2 border-solid border-primary bg-background shadow transition-transform hover:scale-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" />
  </RadixSlider.Root>
);

/* ───────────────────────── Progress ───────────────────────── */

export const PitsProgressBar: React.FC<{
  value: number;
  tone?: 'default' | 'warn' | 'error';
  className?: string;
}> = ({ value, tone = 'default', className }) => {
  const clamped = Math.max(0, Math.min(100, value));
  return (
    <RadixProgress.Root
      value={clamped}
      className={cn(
        'pits-ui relative h-1.5 w-full overflow-hidden rounded-full bg-secondary',
        className,
      )}
    >
      <RadixProgress.Indicator
        className={cn(
          'h-full transition-all duration-300',
          tone === 'error'
            ? 'bg-red-500'
            : tone === 'warn'
              ? 'bg-amber-500'
              : 'bg-primary',
        )}
        style={{ width: `${clamped}%` }}
      />
    </RadixProgress.Root>
  );
};

/* ───────────────────────── Tooltip ───────────────────────── */

export const PitsTooltipProvider = RadixTooltip.Provider;

export const PitsTooltip: React.FC<{
  content: React.ReactNode;
  children: React.ReactElement;
  side?: 'top' | 'right' | 'bottom' | 'left';
}> = ({ content, children, side = 'top' }) => (
  <RadixTooltip.Root>
    <RadixTooltip.Trigger asChild>{children}</RadixTooltip.Trigger>
    <RadixTooltip.Portal>
      <RadixTooltip.Content
        side={side}
        sideOffset={6}
        className="pits-ui z-[70] max-w-xs rounded-md border border-solid border-border bg-popover px-2.5 py-1.5 text-[11px] font-medium text-popover-foreground shadow-md"
      >
        {content}
      </RadixTooltip.Content>
    </RadixTooltip.Portal>
  </RadixTooltip.Root>
);
