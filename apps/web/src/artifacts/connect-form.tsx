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
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import { Input } from '@/components/ui/input';

interface ConnectFormProps {
  /** Optional initial server URL (defaults to empty string for current origin). */
  initialServerUrl?: string;
  /** Receives a non-empty token and server URL (empty string for current origin); the caller verifies it with the server. */
  onConnect: (token: string, serverUrl: string) => void;
}

/** Asks for the access token printed into the server's data directory. */
export function ConnectForm({
  initialServerUrl = '',
  onConnect,
}: ConnectFormProps) {
  const [serverUrl, setServerUrl] = useState(initialServerUrl);
  const [token, setToken] = useState('');
  function submit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmedToken = token.trim();
    if (trimmedToken !== '') onConnect(trimmedToken, serverUrl.trim());
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
        <form onSubmit={submit}>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="server-url">Server URL</FieldLabel>
              <Input
                id="server-url"
                type="text"
                autoComplete="off"
                spellCheck={false}
                placeholder="Current origin (e.g. http://127.0.0.1:7410)"
                value={serverUrl}
                onChange={(event) => setServerUrl(event.target.value)}
              />
              <FieldDescription>
                Leave blank to connect to this page’s origin under{' '}
                <code>/v1</code>, or enter a remote or LAN server address (e.g.{' '}
                <code>http://192.168.1.10:7410</code>).
              </FieldDescription>
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
                The bearer token for API authorization.
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
