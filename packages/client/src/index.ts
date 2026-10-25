import type { ServiceDescription } from '@artifree/contracts';

/** Future HTTP SDK configuration. This scaffold performs no network requests. */
export interface ClientConfiguration {
  readonly baseUrl: string;
  readonly accessToken?: string;
}

/** DTO alias for consumers. A scaffold description is not a health response. */
export type ScaffoldInventory = ServiceDescription;
