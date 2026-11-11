/**
 * The followers the client loads on use. They are one module so that a bundler
 * makes one lazy chunk of them: two lazy chunks that share code with the entry
 * make a bundler split that code into a third chunk, which a page then
 * preloads.
 */
export { followData } from './follow-data.js';
export { followJob } from './follow-job.js';
