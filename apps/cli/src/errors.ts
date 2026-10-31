/**
 * Error thrown when CLI arguments, environment variables, or profiles are invalid.
 */
export class UsageError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'UsageError';
  }
}
