/** Invalid command-line input; the caller reports it with exit code 2. */
export class UsageError extends Error {
  override name = 'UsageError';
}
