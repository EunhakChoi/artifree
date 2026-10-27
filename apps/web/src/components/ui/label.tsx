// Adapted from shadcn/ui (MIT). See THIRD_PARTY_NOTICES.md.
import * as React from 'react';
import { cn } from 'cn';

function Label({
  className,
  htmlFor,
  ...props
}: React.ComponentProps<'label'>) {
  return (
    <label
      htmlFor={htmlFor}
      data-slot="label"
      className={cn(
        'flex items-center gap-2 text-sm leading-none font-medium select-none group-data-[disabled=true]:pointer-events-none group-data-[disabled=true]:opacity-50 peer-disabled:cursor-not-allowed peer-disabled:opacity-50',
        className,
      )}
      {...props}
    />
  );
}

export { Label };
