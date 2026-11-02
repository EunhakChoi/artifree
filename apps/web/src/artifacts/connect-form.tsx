import { useState } from 'react';
import type { FormEvent } from 'react';
import { PlugIcon } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card';
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Input } from '@/components/ui/input';
import { parseServerUrl } from './server-url';

interface ConnectFormProps {
  /** Optional initial server URL (defaults to empty string for current origin). */
  initialServerUrl?: string;
  /** The server an empty field stands for, built into this page; empty when none. */
  defaultServerUrl?: string;
  /** Problems found while choosing the starting server, shown above the form. */
  notices?: readonly string[];
  /** Receives a non-empty token and server URL (empty string for current origin); the caller verifies it with the server. */
  onConnect: (token: string, serverUrl: string) => void;
}

/** Asks for the access token printed into the server's data directory. */
export function ConnectForm({
  initialServerUrl = '',
  defaultServerUrl = '',
  notices = [],
  onConnect,
}: ConnectFormProps) {
  const [serverUrl, setServerUrl] = useState(initialServerUrl);
  const [serverProblem, setServerProblem] = useState<string | undefined>();
  const [token, setToken] = useState('');
  function submit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmedToken = token.trim();
    if (trimmedToken === '') return;
    // An empty field means the default server; anything else must be a
    // plain http(s) address before the token is sent to it.
    if (serverUrl.trim() === '') {
      setServerProblem(undefined);
      onConnect(trimmedToken, '');
      return;
    }
    const checked = parseServerUrl(serverUrl);
    if (!checked.ok) {
      setServerProblem(checked.reason);
      return;
    }
    setServerProblem(undefined);
    onConnect(trimmedToken, checked.url);
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h3>Connect to your server</h3>
        </CardTitle>
        <CardDescription>
          Start <code>artifree-server serve --data-dir &lt;dir&gt;</code>, then
          paste the token from <code>&lt;dir&gt;/access-token</code>. It is kept
          only for this browser tab.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {notices.map((notice) => (
          <Alert key={notice} variant="destructive" className="mb-4">
            <AlertDescription>{notice}</AlertDescription>
          </Alert>
        ))}
        <form onSubmit={submit}>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="server-url">Server URL</FieldLabel>
              <Input
                id="server-url"
                type="text"
                autoComplete="off"
                spellCheck={false}
                placeholder="http://127.0.0.1:7410"
                aria-invalid={serverProblem !== undefined}
                value={serverUrl}
                onChange={(event) => setServerUrl(event.target.value)}
              />
              <FieldDescription>
                {defaultServerUrl === ''
                  ? 'Leave empty to use the current origin.'
                  : `Leave empty to use ${defaultServerUrl}, the default this page was built with.`}{' '}
                Remote servers must have CORS enabled.
              </FieldDescription>
              {serverProblem === undefined ? null : (
                <FieldError>{serverProblem}</FieldError>
              )}
            </Field>
            <Field>
              <FieldLabel htmlFor="access-token">Access token</FieldLabel>
              <Input
                id="access-token"
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={token}
                onChange={(event) => setToken(event.target.value)}
              />
              <FieldDescription>
                Requests go to <code>/v1</code> on the target server.
              </FieldDescription>
            </Field>
            <Field orientation="horizontal">
              <Button type="submit" disabled={token.trim() === ''}>
                <PlugIcon data-icon="inline-start" aria-hidden="true" />
                Connect
              </Button>
            </Field>
          </FieldGroup>
        </form>
      </CardContent>
    </Card>
  );
}
