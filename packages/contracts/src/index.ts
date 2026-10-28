/**
 * Generated transport contracts, a build-time inventory (not live health),
 * and the package rules the server enforces.
 */
import type { Component } from './generated/schema.js';

export type * from './generated/schema.js';
export type * from './generated/api.js';
export { scaffoldDescription } from './generated/inventory.js';
export { packageRules } from './generated/package-rules.js';
export type { PackageRules } from './generated/package-rules.js';

/** Stable public names even when schema documentation changes emitted unions. */
export type Layer = Component['layer'];
export type ComponentStatus = Component['status'];
