/** Generated transport contracts and a build-time inventory, not live health. */
import type { Component } from './generated/schema.js';

export type * from './generated/schema.js';
export { scaffoldDescription } from './generated/inventory.js';

/** Stable public names even when schema documentation changes emitted unions. */
export type Layer = Component['layer'];
export type ComponentStatus = Component['status'];
