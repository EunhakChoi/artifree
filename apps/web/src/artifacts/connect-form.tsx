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
  /** Receives a non-empty token; the caller verifies it with the server. */
  onConnect: (token: string) => void;
}

/** Asks for the access token printed into the server's data directory. */
export function ConnectForm({ onConnect }: ConnectFormProps) {
  const [token, setToken] = useState('');
  function submit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault();
    const trimmed = token.trim();
    if (trimmed !== '') onConnect(trimmed);
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
                Requests go to this page’s origin under <code>/v1</code>; the
                development server forwards them to the API.
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
