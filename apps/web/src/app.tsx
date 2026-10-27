import { useState } from 'react';
import { ArrowUpRightIcon, InfoIcon, MoonIcon, SunIcon } from 'lucide-react';
import { scaffoldDescription } from '@artifree/contracts';
import type { Component, ComponentStatus, Layer } from '@artifree/contracts';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card';
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
import { Separator } from '@/components/ui/separator';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { ArtifactWorkspace } from '@/artifacts/workspace';

const repo = 'https://github.com/EunhakChoi/artifree';
const layers: Layer[] = ['presentation', 'logic', 'provider', 'shared'];
const statusBadges: Record<
  ComponentStatus,
  { label: string; variant: 'default' | 'secondary' | 'outline' }
> = {
  implemented: { label: 'Working', variant: 'default' },
  scaffold: { label: 'Scaffold', variant: 'secondary' },
  planned: { label: 'Planned', variant: 'outline' },
};

function ModuleCard({ component }: { component: Component }) {
  const sourceKind = component.path.endsWith('.rs') ? 'blob' : 'tree';
  return (
    <article aria-labelledby={`module-${component.id}`}>
      <Card className="h-full">
        <CardHeader>
          <CardAction>
            <Badge variant={statusBadges[component.status].variant}>
              {statusBadges[component.status].label}
            </Badge>
          </CardAction>
          <CardTitle>
            <h3 id={`module-${component.id}`}>{component.name}</h3>
          </CardTitle>
          <CardDescription>{component.language}</CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm leading-relaxed">{component.summary}</p>
        </CardContent>
        <CardFooter className="mt-auto">
          <a
            href={`${repo}/${sourceKind}/main/${component.path}`}
            target="_blank"
            rel="noopener noreferrer"
            className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
          >
            <code className="min-w-0 break-all">{component.path}</code>
            <ArrowUpRightIcon className="size-3 shrink-0" aria-hidden="true" />
          </a>
        </CardFooter>
      </Card>
    </article>
  );
}

function FoundationDialog() {
  return (
    <Dialog>
      <DialogTrigger render={<Button variant="outline" />}>
        <InfoIcon data-icon="inline-start" aria-hidden="true" />
        About this foundation
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>What works today</DialogTitle>
          <DialogDescription>
            Artifree is in early development. The module inventory below is
            bundled at build time.
          </DialogDescription>
        </DialogHeader>
        <ul className="flex list-disc flex-col gap-2 pl-5 text-sm leading-relaxed">
          <li>HTML artifacts saved as immutable revisions in local SQLite</li>
          <li>Conditional saves with conflict detection and retry keys</li>
          <li>Sandboxed previews on a separate origin</li>
          <li>
            The same operations from the HTTP API, MCP agents, the CLI, and this
            page
          </li>
        </ul>
        <p className="text-sm leading-relaxed text-muted-foreground">
          Sharing, publishing, structured documents, the runtime SDK, and model
          access are planned.
        </p>
        <DialogFooter>
          <DialogClose render={<Button />}>Got it</DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function App() {
  const [dark, setDark] = useState(false);
  function toggleTheme(): void {
    const next = !dark;
    document.documentElement.classList.toggle('dark', next);
    setDark(next);
  }
  const ThemeIcon = dark ? SunIcon : MoonIcon;
  return (
    <div className="min-h-screen md:grid md:grid-cols-[15rem_minmax(0,1fr)]">
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:top-4 focus:left-4 focus:rounded-lg focus:bg-primary focus:p-3 focus:text-primary-foreground"
      >
        Skip to content
      </a>
      <aside className="flex flex-col gap-8 bg-sidebar p-6 text-sidebar-foreground md:min-h-screen md:p-8">
        <a
          href="#main"
          className="flex items-center gap-3 text-2xl font-semibold tracking-tight"
        >
          <span
            aria-hidden="true"
            className="grid size-9 place-items-center rounded-xl bg-primary font-serif text-2xl text-primary-foreground italic"
          >
            a
          </span>
          artifree
        </a>
        <div className="hidden flex-col gap-4 md:flex">
          <p className="font-mono text-xs tracking-widest text-muted-foreground uppercase">
            Your workspace
          </p>
          <nav aria-label="Workspace">
            <ul className="flex flex-col gap-1">
              <li>
                <a
                  href="#artifacts"
                  className="block rounded-lg bg-sidebar-accent px-3 py-2 text-sm font-medium text-sidebar-accent-foreground"
                >
                  Artifacts
                </a>
              </li>
              <li>
                <a
                  href="#modules"
                  className="block rounded-lg px-3 py-2 text-sm font-medium hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                >
                  Architecture
                </a>
              </li>
            </ul>
          </nav>
        </div>
        <div className="mt-auto hidden flex-col gap-4 md:flex">
          <p className="text-sm leading-relaxed text-muted-foreground">
            Open artifacts.
            <br />
            Any agent.
          </p>
          <Separator />
          <a
            href={repo}
            className="flex items-center gap-2 text-xs text-muted-foreground hover:text-foreground"
          >
            Project on GitHub
            <ArrowUpRightIcon className="size-3" aria-hidden="true" />
          </a>
        </div>
      </aside>
      <main
        id="main"
        tabIndex={-1}
        className="mx-auto w-full min-w-0 max-w-7xl px-6 pb-8 md:px-10 xl:px-14"
      >
        <header className="flex min-h-20 flex-wrap items-center justify-between gap-3 py-4">
          <span className="font-mono text-xs text-muted-foreground">
            Artifree / workspace
          </span>
          <div className="flex items-center gap-3">
            <Badge variant="outline">
              Early development · {scaffoldDescription.version}
            </Badge>
            <Button
              variant="ghost"
              size="icon"
              onClick={toggleTheme}
              aria-label="Toggle color theme"
              aria-pressed={dark}
            >
              <ThemeIcon data-icon="inline-start" aria-hidden="true" />
            </Button>
          </div>
        </header>
        <Separator />
        <section
          className="flex flex-col items-start gap-5 py-10 md:py-14"
          aria-labelledby="workspace-title"
        >
          <p className="font-mono text-xs tracking-widest text-accent-foreground uppercase">
            Your agents. Your artifacts.
          </p>
          <h1
            id="workspace-title"
            className="max-w-3xl text-4xl leading-tight font-semibold tracking-tight sm:text-5xl lg:text-6xl"
          >
            A place for what
            <br />
            your agents{' '}
            <em className="font-serif font-normal text-primary">make.</em>
          </h1>
          <p className="max-w-lg text-sm leading-relaxed text-muted-foreground">
            A workspace you run on your own machine. Agents and people create
            HTML artifacts, save revisions, and preview them in a sandbox.
          </p>
          <FoundationDialog />
        </section>
        <section
          id="artifacts"
          className="flex flex-col gap-5 py-8"
          aria-labelledby="artifacts-title"
        >
          <h2
            id="artifacts-title"
            className="text-xl font-semibold tracking-tight"
          >
            Artifacts
          </h2>
          <ArtifactWorkspace />
        </section>
        <Separator />
        <section id="modules" className="py-8" aria-labelledby="modules-title">
          <Alert className="mb-5">
            <InfoIcon aria-hidden="true" />
            <AlertTitle>Build-time inventory</AlertTitle>
            <AlertDescription>
              Module status is bundled when this page is built; it does not
              report whether a server is running.
            </AlertDescription>
          </Alert>
          <div className="mb-5 flex items-center justify-between gap-4">
            <h2
              id="modules-title"
              className="text-xl font-semibold tracking-tight"
            >
              Inside Artifree
            </h2>
            <span className="font-mono text-xs text-muted-foreground">
              {scaffoldDescription.components.length} modules
            </span>
          </div>
          <Tabs defaultValue="presentation">
            <TabsList
              className="w-full sm:w-fit"
              aria-label="Architecture layers"
            >
              {layers.map((layer) => (
                <TabsTrigger key={layer} value={layer}>
                  {layer}
                </TabsTrigger>
              ))}
            </TabsList>
            {layers.map((layer) => (
              <TabsContent key={layer} value={layer} className="mt-5">
                <div className="grid gap-4 lg:grid-cols-2 2xl:grid-cols-3">
                  {scaffoldDescription.components
                    .filter((component) => component.layer === layer)
                    .map((component) => (
                      <ModuleCard key={component.id} component={component} />
                    ))}
                </div>
              </TabsContent>
            ))}
          </Tabs>
        </section>
        <Separator />
        <footer className="flex flex-wrap gap-x-6 gap-y-2 pt-5 font-mono text-xs text-muted-foreground">
          <span>Presentation · TypeScript</span>
          <span>Logic · Rust</span>
          <span>Server · Axum on Tokio</span>
        </footer>
      </main>
    </div>
  );
}
