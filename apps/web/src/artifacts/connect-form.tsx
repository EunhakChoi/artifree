import { useState } from 'react';
import type { FormEvent } from 'react';
import { PlugIcon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
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
import { Input } from '@/components/ui/input';
import { parseServerUrl } from './server-url';

interface ConnectFormProps {
  /** Optional initial server URL (defaults to empty string for current origin). */
  initialServerUrl?: string;
  /** The server an empty field leads to; empty means the page's own origin. */
  defaultServerUrl?: string;
  /** Why an address was ignored while the page started; shown above the form. */
  notices?: readonly string[];
  /**
   * Receives a non-empty token and a checked server URL, or an empty string
   * for the default server; the caller verifies it with the server.
   */
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
  const [token, setToken] = useState('');
  const [serverError, setServerError] = useState<string>();
  function submit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmedToken = token.trim();
    if (trimmedToken === '') return;
    if (serverUrl.trim() === '') {
      onConnect(trimmedToken, '');
      return;
    }
    // The address is checked before any request; it is not repeated back.
    const result = parseServerUrl(serverUrl);
    if (!result.ok) {
      setServerError(
        `This server address cannot be used because ${result.reason}.`,
      );
      return;
    }
    onConnect(trimmedToken, result.url);
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
        {notices.length > 0 && (
          <Alert className="mb-4">
            <AlertTitle>Server address ignored</AlertTitle>
            <AlertDescription>
              {notices.map((notice) => (
                <p key={notice}>{notice}</p>
              ))}
            </AlertDescription>
          </Alert>
        )}
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
                value={serverUrl}
                aria-invalid={serverError !== undefined}
                onChange={(event) => {
                  setServerUrl(event.target.value);
                  setServerError(undefined);
                }}
              />
              <FieldDescription>
                Leave empty to use{' '}
                {defaultServerUrl === '' ? (
                  'the current origin'
                ) : (
                  <>
                    the address built into this page,{' '}
                    <code>{defaultServerUrl}</code>
                  </>
                )}
                . Remote servers must have CORS enabled.
              </FieldDescription>
              {serverError !== undefined && (
                <FieldError>{serverError}</FieldError>
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
