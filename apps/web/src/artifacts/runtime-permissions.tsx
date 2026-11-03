import type { RuntimeView } from '@artifree/contracts';
import { Button } from '@/components/ui/button';

/**
 * The capabilities a preview can be granted, each with its own control: a
 * grant of one never implies another (decisions 0020 and 0032).
 */
const CAPABILITY_CONTROLS = [
  {
    name: 'metadata.read',
    allow: 'Allow metadata access',
    revoke: 'Revoke metadata access',
  },
  {
    name: 'source.read',
    allow: 'Allow source reading',
    revoke: 'Revoke source reading',
  },
  {
    name: 'source.download',
    allow: 'Allow download requests',
    revoke: 'Revoke download requests',
  },
  {
    name: 'source.write',
    allow: 'Allow source editing',
    revoke: 'Revoke source editing',
    // Shown beside the control, because this grant is the user's only
    // confirmation of what the app then does by itself (decision 0033).
    warning:
      'Allowing this lets the app change files of this artifact. A save creates a new revision without asking you again; earlier revisions stay and can be restored. After a save this preview is out of date, and you must allow editing again for the next one.',
  },
] as const;

interface RuntimePermissionsProps {
  readonly view: RuntimeView;
  /** `settled` when the grant shown is the server's last confirmed one. */
  readonly grantStatus: 'settled' | 'pending' | 'unknown';
  /** Asks for the grant to become exactly `capabilities`. */
  readonly onChange: (capabilities: string[]) => void;
}

/** What a runtime preview declares and has been granted, with one control per capability. */
export function RuntimePermissions({
  view,
  grantStatus,
  onChange,
}: RuntimePermissionsProps) {
  return (
    <section
      aria-label="Runtime permissions"
      className="flex flex-col gap-2 rounded-lg border p-3 text-sm"
    >
      <p>SDK: {view.sdk}</p>
      <p>Declared: {view.declared.join(', ') || 'none'}</p>
      <p>Supported: {view.supported.join(', ') || 'none'}</p>
      {grantStatus === 'settled' ? (
        <p>Granted: {view.granted.join(', ') || 'none'}</p>
      ) : (
        <p>
          {grantStatus === 'pending'
            ? 'Permission change pending.'
            : 'Permission status is unknown. Clear access to resolve it.'}
        </p>
      )}
      {grantStatus === 'unknown' ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onChange([])}
        >
          Resolve by revoking access
        </Button>
      ) : grantStatus === 'settled' ? (
        CAPABILITY_CONTROLS.filter(
          ({ name }) =>
            view.supported.includes(name) && view.declared.includes(name),
        ).map((control) => {
          const { name, allow, revoke } = control;
          const granted = view.granted.includes(name);
          const warning = 'warning' in control ? control.warning : undefined;
          const noteId = `runtime-${name}-note`;
          return (
            <div key={name} className="flex flex-col gap-1">
              <Button
                type="button"
                variant="outline"
                size="sm"
                aria-describedby={warning === undefined ? undefined : noteId}
                onClick={() =>
                  onChange(
                    granted
                      ? view.granted.filter((item) => item !== name)
                      : [...view.granted, name],
                  )
                }
              >
                {granted ? revoke : allow}
              </Button>
              {warning === undefined ? null : (
                <p id={noteId} className="text-xs text-muted-foreground">
                  {warning}
                </p>
              )}
            </div>
          );
        })
      ) : null}
    </section>
  );
}
