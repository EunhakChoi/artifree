// Original component for line comparisons; not part of shadcn/ui.
import * as React from 'react';
import { cva } from 'class-variance-authority';
import { cn } from 'cn';

/** A block of compared lines, read like a unified diff. */
function Diff({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot="diff"
      className={cn(
        'overflow-x-auto rounded-lg border py-1 font-mono text-xs',
        className,
      )}
      {...props}
    />
  );
}

const diffLineVariants = cva(
  'grid grid-cols-[3rem_3rem_1rem_minmax(0,1fr)] gap-x-1 px-2 whitespace-pre-wrap break-all',
  {
    variants: {
      kind: {
        same: 'text-muted-foreground',
        added: 'bg-secondary text-secondary-foreground',
        removed: 'bg-destructive/10 text-destructive',
      },
    },
  },
);

const MARKS = { same: ' ', added: '+', removed: '-' } as const;

/**
 * One compared line with its numbers on each side. The +/- mark carries the
 * change in text, so color is never the only signal.
 */
function DiffLine({
  className,
  kind,
  before,
  after,
  children,
  ...props
}: React.ComponentProps<'div'> & {
  kind: 'same' | 'added' | 'removed';
  before?: number | undefined;
  after?: number | undefined;
}) {
  return (
    <div
      data-slot="diff-line"
      data-kind={kind}
      className={cn(diffLineVariants({ kind }), className)}
      {...props}
    >
      <span className="text-right select-none">{before ?? ''}</span>
      <span className="text-right select-none">{after ?? ''}</span>
      <span className="select-none">{MARKS[kind]}</span>
      <span>{children}</span>
    </div>
  );
}

/** A gap between groups of compared lines. */
function DiffGap({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot="diff-gap"
      className={cn('px-2 text-muted-foreground select-none', className)}
      {...props}
    />
  );
}

export { Diff, DiffGap, DiffLine };
