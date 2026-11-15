/**
 * The sandbox of every preview frame, of a revision's files and of its build
 * (decisions 0003 and 0058, rule 14): scripts, forms and dialogs run, and the
 * frame never shares the workspace's origin.
 *
 * `allow-downloads` is the browser's own flag for a page that starts a
 * download itself. It is unrelated to the `source.download` capability, which
 * asks the workspace to offer a stored file and needs the user's confirmation
 * each time (decision 0032).
 */
export const PREVIEW_SANDBOX =
  'allow-scripts allow-forms allow-modals allow-downloads';
