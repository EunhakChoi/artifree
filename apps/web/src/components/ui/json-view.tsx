import type { ComponentProps } from 'react';
import { cn } from 'cn';

/** Displays JSON as text without interpreting its contents as HTML. */
export function JsonView({ className, ...props }: ComponentProps<'pre'>) {
  return (
    <pre
      className={cn(
        'overflow-auto rounded-md border p-3 text-xs whitespace-pre-wrap',
        className,
      )}
      {...props}
    />
  );
}
