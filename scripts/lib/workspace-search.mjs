/**
 * Starts a fresh title search and returns after input finishes. `settled`
 * observes the exact new request, its answer and rendered result; its rejection
 * stays handled until the caller can wait. This lets a test finish an action
 * that owns the read lane before waiting for the queued search.
 */
export async function beginWorkspaceArtifactSearch(page, title) {
  const workspace = page.locator('#artifacts');
  const search = workspace.getByLabel('Search titles');
  const requests = new WeakSet();
  const answered = page.waitForResponse((response) =>
    requests.has(response.request()),
  );
  const admitted = page.waitForRequest((request) => {
    const url = new URL(request.url());
    const matches =
      request.method() === 'GET' &&
      url.pathname === '/v1/artifacts' &&
      url.searchParams.get('q') === title &&
      request.frame() === page.mainFrame();
    if (matches) requests.add(request);
    return matches;
  });
  // Filling may fail before the waiter rejects. Observe that rejection while
  // preserving the original promise and any earlier input failure.
  void admitted.catch(() => {});
  void answered.catch(() => {});
  await search.fill(title);
  const settled = (async () => {
    // Request effects run after the query's Loading indicator commits. An
    // already listed artifact is not proof that this query rendered.
    await admitted;
    await answered;
    await workspace
      .getByText('Loading artifacts…', { exact: true })
      .waitFor({ state: 'hidden' });
    if (
      await workspace
        .getByText('Could not list artifacts', { exact: true })
        .isVisible()
    )
      throw new Error('The artifact search failed.');
    if ((await search.inputValue()) !== title)
      throw new Error(
        'The artifact search changed before its answer was shown.',
      );
  })();
  // A caller may still own a held read when settlement fails. Observe that
  // failure now without replacing the promise it will eventually consume.
  void settled.catch(() => {});
  return { settled };
}
