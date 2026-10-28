// Original component for line comparisons; not part of shadcn/ui.
import * as React from 'react';
import { cva } from 'class-variance-authority';
import { cn } from 'cn';

/**
 * Lines of two texts side by side in one table: the line's number in each
 * text, a change mark, and the line. `label` names the comparison for
 * assistive technology.
 */
function LineDiff({
  className,
  label,
  children,
  ...props
}: React.ComponentProps<'table'> & { label: string }) {
  return (
    <div
      data-slot="line-diff"
      className="max-h-128 overflow-auto rounded-lg border font-mono text-xs"
    >
      <table className={cn('w-full border-collapse', className)} {...props}>
        <caption className="sr-only">{label}</caption>
        <thead className="sr-only">
          <tr>
            <th scope="col">Earlier line</th>
            <th scope="col">Later line</th>
            <th scope="col">Change</th>
            <th scope="col">Text</th>
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

const lineVariants = cva('align-top', {
  variants: {
    kind: {
      same: 'text-muted-foreground',
      added: 'bg-secondary text-secondary-foreground',
      removed: 'bg-destructive/10 text-destructive',
    },
  },
});

const MARK = { same: '', added: '+', removed: '−' } as const;
const SPOKEN = {
  same: 'unchanged',
  added: 'added',
  removed: 'removed',
} as const;

/**
 * One line. Its change is a visible mark and spoken text, so color is
 * never the only signal.
 */
function LineDiffRow({
  className,
  kind,
  earlier,
  later,
  children,
  ...props
}: React.ComponentProps<'tr'> & {
  kind: 'same' | 'added' | 'removed';
  earlier: number | undefined;
  later: number | undefined;
}) {
  return (
    <tr
      data-slot="line-diff-row"
      data-kind={kind}
      className={cn(lineVariants({ kind }), className)}
      {...props}
    >
      <td className="w-12 px-2 text-right select-none">{earlier}</td>
      <td className="w-12 px-2 text-right select-none">{later}</td>
      <td className="w-6 text-center select-none">
        <span aria-hidden="true">{MARK[kind]}</span>
        <span className="sr-only">{SPOKEN[kind]}</span>
      </td>
      <td className="pr-2 break-all whitespace-pre-wrap">{children}</td>
    </tr>
  );
}

/** Unchanged lines that the comparison leaves out. */
function LineDiffGap({ className, ...props }: React.ComponentProps<'td'>) {
  return (
    <tr data-slot="line-diff-gap">
      <td
        colSpan={4}
        className={cn('bg-muted px-2 py-1 text-muted-foreground', className)}
        {...props}
      />
    </tr>
  );
}

export { LineDiff, LineDiffGap, LineDiffRow };
