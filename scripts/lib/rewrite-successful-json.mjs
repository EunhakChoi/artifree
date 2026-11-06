/** Rewrites a successful routed answer while passing HTTP failures through. */
export async function rewriteSuccessfulJson(route, change) {
  const response = await route.fetch();
  if (!response.ok()) return route.fulfill({ response });
  return route.fulfill({ json: change(await response.json()) });
}
