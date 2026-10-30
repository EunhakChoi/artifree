import type { ArtifactRuntime } from '@artifree/runtime-sdk';

// This file is compiled as a consumer of the package's public entry point.
const runtime: ArtifactRuntime | undefined = window.artifree;
void runtime?.currentMetadata();
