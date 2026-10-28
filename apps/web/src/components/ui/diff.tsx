// Original component for line comparisons; not part of shadcn/ui.
import * as React from 'react';
import { cva } from 'class-variance-authority';
import { cn } from 'cn';

/**
 * A line comparison as a table: the line number before and after, a
 * change mark, and the text. `caption` names the compared file.
 */
function Diff({
  className,
  caption,
  children,
  ...props
}: React.ComponentProps<'table'> & { caption: string }) {
  return (
    <div
      data-slot="diff"
      className="overflow-x-auto rounded-lg border font-mono text-xs"
    >
      <table className={cn('w-full border-collapse', className)} {...props}>
        <caption className="sr-only">{caption}</caption>
        <thead className="sr-only">
          <tr>
            <th scope="col">Before</th>
            <th scope="col">After</th>
            <th scope="col">Change</th>
            <th scope="col">Line</th>
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

const diffRowVariants = cva('align-top', {
  variants: {
    kind: {
      same: 'text-muted-foreground',
      added: 'bg-secondary text-secondary-foreground',
      removed: 'bg-destructive/10 text-destructive',
    },
  },
});

const MARKS = { same: '', added: '+', removed: '-' } as const;

/**
 * One compared line. The mark carries the change in text, so color is
 * never the only signal.
 */
function DiffRow({
  className,
  kind,
  before,
  after,
  children,
  ...props
}: React.ComponentProps<'tr'> & {
  kind: 'same' | 'added' | 'removed';
  before: number | undefined;
  after: number | undefined;
}) {
  return (
    <tr
      data-slot="diff-row"
      data-kind={kind}
      className={cn(diffRowVariants({ kind }), className)}
      {...props}
    >
      <td className="w-12 px-2 text-right select-none">{before}</td>
      <td className="w-12 px-2 text-right select-none">{after}</td>
      <td className="w-4 select-none">{MARKS[kind]}</td>
      <td className="pr-2 break-all whitespace-pre-wrap">{children}</td>
    </tr>
  );
}

/** Unchanged lines left out between two groups. */
function DiffGap({ className, ...props }: React.ComponentProps<'td'>) {
  return (
    <tr data-slot="diff-gap">
      <td
        colSpan={4}
        className={cn('px-2 text-muted-foreground', className)}
        {...props}
      />
    </tr>
  );
}

export { Diff, DiffGap, DiffRow };
