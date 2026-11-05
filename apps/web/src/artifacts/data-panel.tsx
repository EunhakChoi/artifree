import type { ArtifreeClient } from '@artifree/client';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { JsonView } from '@/components/ui/json-view';
import { usePageVisible } from './page-visibility';
import { useRequest } from './use-request';

interface DataPanelProps {
  client: ArtifreeClient;
  artifactId: string;
  resets: number;
}

/** A snapshot of the artifact's application data, read only while visible. */
export function DataPanel(props: DataPanelProps) {
  const visible = usePageVisible();
  return visible ? <DataSnapshot {...props} key={props.resets} /> : null;
}

function DataSnapshot({ client, artifactId, resets }: DataPanelProps) {
  const data = useRequest(`${artifactId}:${resets}`, (signal) =>
    client.getData(artifactId, undefined, { signal }),
  );
  if (data.status === 'loading') return <output>Loading data…</output>;
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
        <span>Snapshot</span>
      </div>
      <p>
        This snapshot does not update automatically. Reopen the Data tab to read
        again.
      </p>
      <JsonView className="max-h-96">
        {JSON.stringify(data.value.value, null, 2)}
      </JsonView>
    </div>
  );
}
