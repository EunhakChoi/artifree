import { useState } from 'react';
import type { FormEvent } from 'react';
import { PlusIcon } from 'lucide-react';
import { newIdempotencyKey } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, CreateArtifactRequest } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Field, FieldGroup, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { describeFailure } from './use-request';

const STARTER = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>New artifact</title>
  </head>
  <body>
    <h1>Hello from Artifree</h1>
  </body>
</html>
`;

interface CreateDialogProps {
  client: ArtifreeClient;
  /** Prevents opening the dialog, for example while a draft is unsaved. */
  disabled: boolean;
  onCreated: (artifact: Artifact) => void;
}

/** Creates an artifact with a single `index.html`. */
export function CreateDialog({
  client,
  disabled,
  onCreated,
}: CreateDialogProps) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState('');
  const [html, setHtml] = useState(STARTER);
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<string>();
  // The last request sent and its key. Sending the same content again reuses
  // the key, so a retry after a lost response returns the original artifact
  // instead of creating another, even if the text was edited and restored.
  const [attempt, setAttempt] = useState<{ body: string; key: string }>();

  function edit(update: () => void): void {
    update();
    setFailure(undefined);
  }

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const request: CreateArtifactRequest = {
      title: title.trim(),
      files: [{ path: 'index.html', encoding: 'utf8', content: html }],
    };
    const body = JSON.stringify(request);
    const key = attempt?.body === body ? attempt.key : newIdempotencyKey();
    setAttempt({ body, key });
    setPending(true);
    setFailure(undefined);
    try {
      const result = await client.createArtifact(request, {
        idempotencyKey: key,
      });
      setOpen(false);
      setTitle('');
      setHtml(STARTER);
      setAttempt(undefined);
      onCreated(result.artifact);
    } catch (error: unknown) {
      setFailure(describeFailure(error));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // Stay open until the request settles: its result changes the
        // selection, and the form shows its outcome.
        if (pending) return;
        if (next) setFailure(undefined);
        setOpen(next);
      }}
    >
      <DialogTrigger render={<Button disabled={disabled} />}>
        <PlusIcon data-icon="inline-start" aria-hidden="true" />
        New artifact
      </DialogTrigger>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => void submit(event)}
        >
          <DialogHeader>
            <DialogTitle>New HTML artifact</DialogTitle>
            <DialogDescription>
              Saved as revision 1 with one <code>index.html</code>. Agents can
              add more files through the API or CLI.
            </DialogDescription>
          </DialogHeader>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="new-title">Title</FieldLabel>
              <Input
                id="new-title"
                required
                maxLength={200}
                // Read-only while creating: the request carries these values,
                // and the form resets when it succeeds.
                readOnly={pending}
                value={title}
                onChange={(event) => edit(() => setTitle(event.target.value))}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="new-html">index.html</FieldLabel>
              <Textarea
                id="new-html"
                variant="code"
                className="max-h-64"
                spellCheck={false}
                readOnly={pending}
                aria-busy={pending}
                value={html}
                onChange={(event) => edit(() => setHtml(event.target.value))}
              />
            </Field>
          </FieldGroup>
          {failure === undefined ? null : (
            <Alert variant="destructive">
              <AlertTitle>Not created</AlertTitle>
              <AlertDescription>{failure}</AlertDescription>
            </Alert>
          )}
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline" disabled={pending} />}
            >
              Cancel
            </DialogClose>
            <Button type="submit" disabled={pending || title.trim() === ''}>
              {pending ? 'Creating…' : 'Create'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
