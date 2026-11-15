import { useState } from 'react';
import type { FormEvent } from 'react';
import { PlusIcon } from 'lucide-react';
import { newIdempotencyKey } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type {
  Artifact,
  CreateArtifactRequest,
  TypeList,
  TypeSummary,
} from '@artifree/contracts';
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
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { Textarea } from '@/components/ui/textarea';
import { describeFailure, useRequest } from './use-request';
import type { RequestState } from './use-request';

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

/** The choice of what revision 1 starts from: one `index.html`. */
const HTML_START = '';

/**
 * Creates an artifact with a single `index.html`, or from a built-in type
 * with the type's starter as revision 1 (decision 0059 rules 5 and 14).
 */
export function CreateDialog({
  client,
  disabled,
  onCreated,
}: CreateDialogProps) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState('');
  const [html, setHtml] = useState(STARTER);
  // A type id, or HTML_START for one index.html.
  const [start, setStart] = useState(HTML_START);
  // The catalog is read each time the dialog opens; without it only the
  // index.html start is offered.
  const types = useRequest<TypeList>(open ? 'types' : undefined, (signal) =>
    client.listTypes({ signal }),
  );
  const offered: readonly TypeSummary[] =
    types.status === 'ready' ? types.value.types : [];
  const chosen = offered.find((type) => type.id === start);
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
    const request: CreateArtifactRequest =
      chosen === undefined
        ? {
            title: title.trim(),
            files: [{ path: 'index.html', encoding: 'utf8', content: html }],
          }
        : { title: title.trim(), type: chosen.id };
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
      setStart(HTML_START);
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
              Saved as revision 1, with one <code>index.html</code> or with the
              starter files of a built-in type. Agents can add more files
              through the API or CLI.
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
              <FieldLabel htmlFor="new-start">Start from</FieldLabel>
              <NativeSelect
                id="new-start"
                disabled={pending}
                value={chosen === undefined ? HTML_START : chosen.id}
                onChange={(event) => edit(() => setStart(event.target.value))}
              >
                <NativeSelectOption value={HTML_START}>
                  One index.html
                </NativeSelectOption>
                {offered.map((type) => (
                  <NativeSelectOption key={type.id} value={type.id}>
                    {type.title} ({type.id}, release {type.release})
                  </NativeSelectOption>
                ))}
              </NativeSelect>
              <TypeNote types={types} chosen={chosen} />
            </Field>
            {chosen === undefined ? (
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
            ) : null}
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

/**
 * What the chosen start means: a type's description, starter, required files
 * and whether this server builds it; or, for one index.html, why no type is
 * offered when the catalog could not be read.
 */
function TypeNote({
  types,
  chosen,
}: {
  readonly types: RequestState<TypeList>;
  readonly chosen: TypeSummary | undefined;
}) {
  if (chosen === undefined) {
    if (types.status === 'loading')
      return <FieldDescription>Reading the built-in types…</FieldDescription>;
    if (types.status === 'error')
      return (
        <FieldDescription>
          The built-in types could not be read ({types.message}); one index.html
          can still be created.
        </FieldDescription>
      );
    return null;
  }
  return (
    <FieldDescription>
      {chosen.description} Starts with {chosen.starter.join(', ')}. Every
      revision must keep {chosen.required.join(', ')}.{' '}
      {chosen.build_profile === null
        ? null
        : chosen.build_available
          ? `Built with the ${chosen.build_profile} profile from the Jobs tab.`
          : `This server cannot build the ${chosen.build_profile} profile; the artifact can still be created and edited.`}
    </FieldDescription>
  );
}
