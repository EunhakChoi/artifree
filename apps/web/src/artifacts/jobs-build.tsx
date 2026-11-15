/**
 * The head build status above the Jobs tab's list (decision 0058, rule 16, and
 * R03ea): the head revision named as the newest, its status, the failure in
 * words and the job that sets it, and the latest build's revision when it is
 * not the head. Diagnostics are shown in the failed build's row only.
 */
import { RotateCwIcon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import type { BuildStatusState } from './jobs-model';
import {
  PROFILE_UNKNOWN_TEXT,
  headStatusLines,
  headStatusTitle,
} from './jobs-text';

/** The props of the head build status. */
export interface BuildStatusBannerProps {
  /** What the tab knows of the head build status. */
  readonly state: BuildStatusState;
  /** Reads the status again, after a failed read. */
  readonly onRetry: () => void;
}

/** The head build status, or why it is not known. */
export function BuildStatusBanner({ state, onRetry }: BuildStatusBannerProps) {
  switch (state.kind) {
    case 'unread':
      return (
        <output className="text-sm text-muted-foreground">
          Reading the build status…
        </output>
      );
    case 'read':
      return (
        <Alert>
          <AlertTitle>{headStatusTitle(state.status)}</AlertTitle>
          <AlertDescription>
            {headStatusLines(state.status).map((line) => (
              <p key={line}>{line}</p>
            ))}
          </AlertDescription>
        </Alert>
      );
    case 'profile_unknown':
      return (
        <Alert>
          <AlertDescription>
            <p>{PROFILE_UNKNOWN_TEXT}</p>
          </AlertDescription>
        </Alert>
      );
    case 'failed':
      return (
        <Alert variant="destructive">
          <AlertTitle>Could not read the build status</AlertTitle>
          <AlertDescription>
            <p>{state.message}</p>
          </AlertDescription>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onRetry}>
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </div>
        </Alert>
      );
    default: {
      const unhandled: never = state;
      throw new Error(`Unhandled build status: ${JSON.stringify(unhandled)}`);
    }
  }
}
