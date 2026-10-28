import { useEffect, useEffectEvent, useState } from 'react';
import type { ReactNode } from 'react';
import { PinIcon } from 'lucide-react';
import type { ArtifreeClient } from '@artifree/client';
import type { Artifact, ListSort } from '@artifree/contracts';
import { Button } from '@/components/ui/button';
import { Field, FieldLabel } from '@/components/ui/field';
import { Input } from '@/components/ui/input';
import {
  NativeSelect,
  NativeSelectOption,
} from '@/components/ui/native-select';
import { isShown, shownBy, showing } from './list-query';
import type { ListQuery } from './list-query';

/** The most one list request returns (decision 0008). */
const REQUEST_LIMIT = 500;
/** How long typing pauses before the list is searched. */
const SEARCH_DELAY_MS = 300;

/** The first `count` artifacts `query` selects, read afresh. */
export interface ListedArtifacts {
  readonly artifacts: readonly Artifact[];
  /** True when no artifact follows the ones read. */
  readonly complete: boolean;
}

/**
 * Reads the first `count` artifacts of `query` from the start, following
 * pages when `count` exceeds one request. Reading from the start each time
 * keeps the list one consistent prefix; continuing an older page token
 * after changes could skip or repeat artifacts (decision 0008).
 */
export async function readArtifacts(
  client: ArtifreeClient,
  query: ListQuery,
  count: number,
  signal: AbortSignal,
): Promise<ListedArtifacts> {
  const artifacts: Artifact[] = [];
  let token: string | undefined;
  do {
    const page = await client.listArtifacts(
      {
        q: query.search === '' ? null : query.search,
        sort: query.sort,
        pinned: query.pinned ?? null,
        deleted: query.deleted,
        limit: Math.min(REQUEST_LIMIT, count - artifacts.length),
        page_token: token ?? null,
      },
      { signal },
    );
    artifacts.push(...page.artifacts);
    token = page.next_page_token ?? undefined;
  } while (token !== undefined && artifacts.length < count);
  return { artifacts, complete: token === undefined };
}

function isSort(value: string): value is ListSort {
  return value === 'updated' || value === 'created';
}

interface ListControlsProps {
  query: ListQuery;
  onQueryChange: (query: ListQuery) => void;
  /**
   * Whether the trash cannot be shown: showing it closes the open artifact,
   * so not while the editor holds a draft (decision 0014, item 3).
   */
  trashLocked: boolean;
}

/** Search, order, and what the artifact list shows. */
export function ListControls({
  query,
  onQueryChange,
  trashLocked,
}: ListControlsProps) {
  const [text, setText] = useState(query.search);
  const search = useEffectEvent((next: string) =>
    onQueryChange({ ...query, search: next }),
  );
  // Searches once typing pauses, so each keystroke is not a request.
  useEffect(() => {
    const next = text.trim();
    if (next === query.search) return undefined;
    const timer = setTimeout(() => search(next), SEARCH_DELAY_MS);
    return () => clearTimeout(timer);
  }, [text, query.search]);
  return (
    <div className="flex flex-col gap-3">
      <Field>
        <FieldLabel htmlFor="artifact-search">Search titles</FieldLabel>
        <Input
          id="artifact-search"
          type="search"
          maxLength={200}
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      </Field>
      <div className="grid grid-cols-2 gap-3">
        <Field>
          <FieldLabel htmlFor="artifact-sort">Order</FieldLabel>
          <NativeSelect
            id="artifact-sort"
            value={query.sort}
            onChange={(event) => {
              const sort = event.target.value;
              if (isSort(sort)) onQueryChange({ ...query, sort });
            }}
          >
            <NativeSelectOption value="updated">
              Recently updated
            </NativeSelectOption>
            <NativeSelectOption value="created">
              Recently created
            </NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field>
          <FieldLabel htmlFor="artifact-shown">Show</FieldLabel>
          <NativeSelect
            id="artifact-shown"
            value={shownBy(query)}
            onChange={(event) => {
              const shown = event.target.value;
              if (isShown(shown)) onQueryChange(showing(query, shown));
            }}
          >
            <NativeSelectOption value="all">All</NativeSelectOption>
            <NativeSelectOption value="pinned">Pinned</NativeSelectOption>
            <NativeSelectOption value="unpinned">Not pinned</NativeSelectOption>
            <NativeSelectOption value="trash" disabled={trashLocked}>
              Trash
            </NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </div>
  );
}

interface ArtifactNavProps {
  artifacts: readonly Artifact[];
  selectedId: string | undefined;
  /** While a draft exists, only the open artifact can be chosen. */
  locked: boolean;
  onSelect: (id: string) => void;
  /** Shown instead of the items when nothing matches. */
  empty: ReactNode;
  /** Offered when more artifacts follow the ones shown. */
  onShowMore: (() => void) | undefined;
  children?: ReactNode;
}

/** The listed artifacts; the open one is marked current. */
export function ArtifactNav({
  artifacts,
  selectedId,
  locked,
  onSelect,
  empty,
  onShowMore,
  children,
}: ArtifactNavProps) {
  return (
    <nav aria-label="Artifacts" className="flex flex-col gap-2">
      {children}
      {artifacts.length === 0 ? empty : null}
      {artifacts.map((artifact) => (
        <Button
          key={artifact.id}
          variant={artifact.id === selectedId ? 'secondary' : 'ghost'}
          aria-current={artifact.id === selectedId ? 'true' : undefined}
          className="justify-between"
          disabled={locked && artifact.id !== selectedId}
          onClick={() => onSelect(artifact.id)}
        >
          <span className="truncate">{artifact.title}</span>
          <span className="flex items-center gap-1 text-xs text-muted-foreground">
            {artifact.pinned ? (
              <>
                <PinIcon aria-hidden="true" />
                <span className="sr-only">Pinned,</span>
              </>
            ) : null}
            r{artifact.current_revision}
          </span>
        </Button>
      ))}
      {onShowMore === undefined ? null : (
        <Button type="button" variant="outline" onClick={onShowMore}>
          Show more
        </Button>
      )}
    </nav>
  );
}
