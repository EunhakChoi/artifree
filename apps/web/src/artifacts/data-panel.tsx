import { useEffect, useState } from 'react';
import type { ArtifreeClient } from '@artifree/client';
import type { AppData } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { JsonView } from '@/components/ui/json-view';
import { usePageVisible } from './page-visibility';
import { describeFailure } from './use-request';

interface DataPanelProps {
  client: ArtifreeClient;
  artifactId: string;
  resets: number;
}

/** Follows the artifact's application data only while the panel is visible. */
export function DataPanel(props: DataPanelProps) {
  const visible = usePageVisible();
  return visible ? <DataFollower {...props} key={props.resets} /> : null;
}

type DataState =
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly value: AppData }
  | { readonly status: 'unavailable' }
  | { readonly status: 'error'; readonly message: string };

function DataFollower({ client, artifactId }: DataPanelProps) {
  const [data, setData] = useState<DataState>({ status: 'loading' });
  useEffect(() => {
    const controller = new AbortController();
    void client
      .followData(artifactId, {
        signal: controller.signal,
        onData(event) {
          if (controller.signal.aborted) return;
          setData(
            event.type === 'unavailable'
              ? { status: 'unavailable' }
              : { status: 'ready', value: event.data },
          );
        },
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted)
          setData({ status: 'error', message: describeFailure(error) });
      });
    return () => controller.abort();
  }, [artifactId, client]);
  if (data.status === 'loading') return <output>Loading data…</output>;
  if (data.status === 'unavailable')
    return (
      <Alert>
        <AlertTitle>Data unavailable</AlertTitle>
        <AlertDescription>
          This artifact is no longer available.
        </AlertDescription>
      </Alert>
    );
  if (data.status === 'error')
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not load data</AlertTitle>
        <AlertDescription>{data.message}</AlertDescription>
      </Alert>
    );
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Badge variant="secondary">
          Data version {data.value.data_version}
        </Badge>
      </div>
      <p>Data follows changes while this tab and page are visible.</p>
      <JsonView className="max-h-96">
        {JSON.stringify(data.value.value, null, 2)}
      </JsonView>
    </div>
  );
}
