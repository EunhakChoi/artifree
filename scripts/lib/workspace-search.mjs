/**
 * Applies a fresh workspace title search before the caller chooses a result.
 * An already listed artifact is not evidence that the debounced query ran.
 * Request effects run after the query's Loading indicator commits. Observe the
 * exact new request's answer, then its rendered result before choosing.
 */
export async function searchWorkspaceArtifacts(page, title) {
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
    throw new Error('The artifact search changed before its answer was shown.');
}
