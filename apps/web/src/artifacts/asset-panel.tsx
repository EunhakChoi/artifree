import { useState } from 'react';
import { DownloadIcon, RotateCwIcon } from 'lucide-react';
import { ArtifreeApiError, listAssets, readAsset } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import type { Asset, AssetList } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from '@/components/ui/empty';
import {
  Meter,
  MeterIndicator,
  MeterLabel,
  MeterTrack,
  MeterValue,
} from '@/components/ui/meter';
import { downloadName, formatBytes, refusalText } from './asset-attempt';
import { AssetDeleteButton, AssetUpload } from './asset-writes-view';
import { pageSaver } from './save-blob';
import type { AssetWrites } from './use-asset-writes';
import { useRequest } from './use-request';

/** How many leading hex characters name an asset or digest on the page. */
const PREFIX = 12;

/** An asset list with the reset count when it was read. */
interface Listed {
  readonly list: AssetList;
  readonly resets: number;
}

interface AssetPanelProps {
  client: ArtifreeClient;
  artifactId: string;
  writes: AssetWrites;
  /** How often the server's data may have been replaced so far. */
  resets: number;
}

/**
 * The artifact's uploaded files (decision 0045, rule 11): usage, the list,
 * downloads as attachments, uploads and deletes. Content is never shown,
 * opened or run on this page; it is only handed to the browser to save.
 * The list is read again after every upload or delete, since a receipt says
 * what happened then, not what is there now. The outcome of the last write
 * is shown by the caller, so it stays reachable when this panel is not.
 */
export function AssetPanel({
  client,
  artifactId,
  writes,
  resets,
}: AssetPanelProps) {
  // A failed read is retried by the user.
  const [generation, setGeneration] = useState(0);
  const listed = useRequest<Listed>(
    `${artifactId}:${writes.listEpoch}:${resets}:${generation}`,
    async (signal) => {
      try {
        return {
          list: await listAssets(client, artifactId, { signal }),
          resets,
        };
      } catch (error: unknown) {
        // Fixed words per code: a missing and a foreign artifact read alike.
        throw new Error(refusalText(error), { cause: error });
      }
    },
    // The rows, a download failure and an open delete dialog stay mounted
    // while the list is read again; writes wait for the new snapshot.
    { keepPrevious: true },
  );
  const shown = listed.status === 'ready' ? listed.value : undefined;
  // The base of the snapshot shown, or none while it is being replaced.
  const base =
    listed.status === 'ready' && !listed.refreshing
      ? listed.value.list.asset_version
      : undefined;
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        Files are kept with this artifact and downloaded as attachments; they
        are never opened or shown on this page. Artifacts cannot read these
        files yet.
      </p>
      {listed.status === 'loading' ? (
        <output className="text-sm text-muted-foreground">
          Loading assets…
        </output>
      ) : listed.status === 'error' ? (
        <Alert variant="destructive">
          <AlertTitle>Could not load the assets</AlertTitle>
          <AlertDescription>
            <p>{listed.message}</p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setGeneration((value) => value + 1)}
            >
              <RotateCwIcon data-icon="inline-start" aria-hidden="true" />
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <Usage list={listed.value.list} />
      )}
      <AssetUpload
        writes={writes}
        base={base}
        shownResets={shown?.resets ?? resets}
      />
      {shown === undefined ? null : (
        <AssetRows
          client={client}
          artifactId={artifactId}
          writes={writes}
          listed={shown}
          base={base}
        />
      )}
    </div>
  );
}

/** Usage of this artifact's space against its limits, from one list read. */
function Usage({ list }: { list: AssetList }) {
  const bytes = `${formatBytes(list.bytes)} of ${formatBytes(list.max_bytes)}`;
  const files = `${list.assets.length} of ${list.max_assets} files`;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Badge variant="secondary">Asset version {list.asset_version}</Badge>
      </div>
      <Meter
        value={list.bytes}
        max={list.max_bytes}
        getAriaValueText={() => bytes}
      >
        <MeterLabel>Storage</MeterLabel>
        <MeterValue>{() => bytes}</MeterValue>
        <MeterTrack>
          <MeterIndicator />
        </MeterTrack>
      </Meter>
      <Meter
        value={list.assets.length}
        max={list.max_assets}
        getAriaValueText={() => files}
      >
        <MeterLabel>Files</MeterLabel>
        <MeterValue>{() => files}</MeterValue>
        <MeterTrack>
          <MeterIndicator />
        </MeterTrack>
      </Meter>
      <p className="text-sm text-muted-foreground">
        Each file counts at its full size, even when it holds the same bytes as
        another. The limit of 256 files and 100 MiB across all your artifacts is
        checked by the server when you upload.
      </p>
    </div>
  );
}

type Download =
  | { readonly status: 'idle' }
  | { readonly status: 'loading'; readonly assetId: string }
  | {
      readonly status: 'failed';
      readonly assetId: string;
      readonly message: string;
    };

/** Why a download failed, in fixed words. */
function downloadFailure(error: unknown): string {
  // A 404 here may also mean the file was deleted since the list was read.
  if (error instanceof ArtifreeApiError && error.status === 404)
    return 'This file cannot be read: it or its artifact may have been removed.';
  return refusalText(error);
}

interface AssetRowsProps {
  client: ArtifreeClient;
  artifactId: string;
  writes: AssetWrites;
  listed: Listed;
  /** The asset version writes are sent against, or none while re-reading. */
  base: number | undefined;
}

function AssetRows({
  client,
  artifactId,
  writes,
  listed,
  base,
}: AssetRowsProps) {
  const [download, setDownload] = useState<Download>({ status: 'idle' });
  const { list } = listed;

  async function save(asset: Asset): Promise<void> {
    setDownload({ status: 'loading', assetId: asset.id });
    try {
      const read = await readAsset(client, artifactId, asset.id);
      // An opaque type, so the browser saves the bytes rather than showing them.
      pageSaver.save(
        read.bytes,
        downloadName(asset.id),
        'application/octet-stream',
      );
      setDownload({ status: 'idle' });
    } catch (error: unknown) {
      setDownload({
        status: 'failed',
        assetId: asset.id,
        message: downloadFailure(error),
      });
    }
  }

  if (list.assets.length === 0)
    return (
      <Empty>
        <EmptyHeader>
          <EmptyTitle>No assets</EmptyTitle>
          <EmptyDescription>
            Files uploaded to this artifact are listed here.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  return (
    <div className="flex flex-col gap-3">
      {download.status === 'failed' ? (
        <Alert variant="destructive">
          <AlertTitle>
            Could not download asset {download.assetId.slice(0, PREFIX)}
          </AlertTitle>
          <AlertDescription>{download.message}</AlertDescription>
        </Alert>
      ) : null}
      <ul aria-label="Assets" className="flex flex-col gap-2">
        {list.assets.map((asset) => {
          const label = asset.id.slice(0, PREFIX);
          const loading =
            download.status === 'loading' && download.assetId === asset.id;
          return (
            <li key={asset.id}>
              <Card size="sm">
                <CardContent>
                  <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
                    <code title={asset.id}>{label}</code>
                    <Badge variant="outline">{asset.content_type}</Badge>
                    <span>{formatBytes(asset.size_bytes)}</span>
                    <CreatedAt ms={asset.created_at_ms} />
                    <span className="text-muted-foreground">
                      SHA-256{' '}
                      <code title={asset.sha256}>
                        {asset.sha256.slice(0, PREFIX)}
                      </code>
                    </span>
                    <div className="ml-auto flex gap-2">
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={download.status === 'loading'}
                        aria-label={`Download asset ${label}`}
                        onClick={() => void save(asset)}
                      >
                        <DownloadIcon
                          data-icon="inline-start"
                          aria-hidden="true"
                        />
                        {loading ? 'Downloading…' : 'Download'}
                      </Button>
                      <AssetDeleteButton
                        writes={writes}
                        assetId={asset.id}
                        label={label}
                        base={base}
                        shownResets={listed.resets}
                      />
                    </div>
                  </div>
                </CardContent>
              </Card>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** When an asset was created; a time outside the date range is shown as is. */
function CreatedAt({ ms }: { ms: number }) {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return <span>Created at {ms} ms</span>;
  return <time dateTime={date.toISOString()}>{date.toLocaleString()}</time>;
}
