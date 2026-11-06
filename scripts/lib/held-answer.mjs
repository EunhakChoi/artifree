/** Holds the next matching answer; `seen` settles after the server answers. */
export async function holdAnswer(tab, method, match) {
  let release;
  const released = new Promise((resolve) => {
    release = resolve;
  });
  let answered;
  let failed;
  const seen = new Promise((resolve, reject) => {
    answered = resolve;
    failed = reject;
  });
  let taken = false;
  const matches = (url) => match(url.pathname);
  const handler = async (route) => {
    if (taken || route.request().method() !== method) return route.fallback();
    taken = true;
    let response;
    try {
      response = await route.fetch();
    } catch (error) {
      failed(error);
      await route.abort().catch(() => {});
      return undefined;
    }
    // The server has answered; the page sees the answer only later.
    answered(response);
    await released;
    await route.fulfill({ response }).catch(() => {});
    return undefined;
  };
  // Kept until the context closes: once taken, it lets requests pass.
  await tab.route(matches, handler);
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`No ${method} answer was held within 10 s.`)),
      10_000,
    );
  });
  const answer = Promise.race([seen, late]).finally(() => clearTimeout(timer));
  // A click can still be awaited when fetching fails. Observe that early
  // rejection while preserving the original promise for the caller.
  void answer.catch(() => {});
  return { seen: answer, release };
}
