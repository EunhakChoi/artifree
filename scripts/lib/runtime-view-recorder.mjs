/**
 * Records the real view response before fetch delivers it to the workspace.
 * Unlike Playwright's response.json(), this reads the browser's response
 * stream rather than fetching another copy through the debugging protocol.
 * The original response is left for the application to read and validate.
 */
export async function recordRuntimeViews(page) {
  await page.addInitScript(() => {
    if (window !== window.top) return;
    globalThis.recordedRuntimeViews = { views: [], errors: [] };
    const send = window.fetch.bind(window);
    window.fetch = async (input, init) => {
      const response = await send(input, init);
      const requestUrl = input instanceof Request ? input.url : String(input);
      const method =
        init?.method ?? (input instanceof Request ? input.method : 'GET');
      const path = new URL(requestUrl, location.href).pathname;
      if (
        method.toUpperCase() === 'POST' &&
        path.endsWith('/runtime-views') &&
        response.ok
      ) {
        try {
          const view = await response.clone().json();
          globalThis.recordedRuntimeViews.views.push(view);
        } catch (error) {
          globalThis.recordedRuntimeViews.errors.push({
            path,
            status: response.status,
            message: error instanceof Error ? error.message : String(error),
          });
        }
      }
      return response;
    };
  });
  return async () => {
    const recorded = await page.evaluate(() => globalThis.recordedRuntimeViews);
    if (recorded.errors.length !== 0)
      throw new Error(
        `Runtime view recording failed: ${JSON.stringify(recorded.errors)}`,
      );
    return recorded.views;
  };
}
