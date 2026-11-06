/**
 * Waits for this open popup's own finite entry animations, before measuring its
 * settled appearance. Descendant animations (for example spinners) are excluded.
 * Closing, cancellation, infinite animations and the deadline reject the check.
 */
export async function waitForPopupEntrance(popup, { timeoutMs = 5_000 } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0)
    throw new Error(
      'Popup animation deadline must be a positive safe integer.',
    );
  let timer;
  try {
    await Promise.race([
      popup.evaluate(async (element, limit) => {
        const ensureOpen = () => {
          if (
            !element.isConnected ||
            !element.hasAttribute('data-open') ||
            element.hasAttribute('data-closed')
          )
            throw new Error(
              'Popup closed before its entry animations finished.',
            );
        };
        ensureOpen();
        const animations = element.getAnimations();
        for (const animation of animations) {
          const timing = animation.effect?.getComputedTiming();
          if (timing === undefined || !Number.isFinite(timing.endTime))
            throw new Error(
              'Popup entry animations must have a finite end time.',
            );
        }
        let observer;
        let deadline;
        try {
          await new Promise((resolve, reject) => {
            observer = new MutationObserver(() => {
              try {
                ensureOpen();
              } catch (error) {
                reject(error);
              }
            });
            observer.observe(element, {
              attributes: true,
              attributeFilter: ['data-open', 'data-closed'],
            });
            observer.observe(element.ownerDocument, {
              childList: true,
              subtree: true,
            });
            deadline = setTimeout(() => {
              reject(new Error(`Popup entry animations exceeded ${limit}ms.`));
            }, limit);
            Promise.all(animations.map((animation) => animation.finished)).then(
              resolve,
              (cause) =>
                reject(
                  new Error('Popup entry animation was cancelled or failed.', {
                    cause,
                  }),
                ),
            );
          });
          ensureOpen();
        } finally {
          clearTimeout(deadline);
          observer?.disconnect();
        }
      }, timeoutMs),
      // Bound locator acquisition and an unresponsive page as well as the
      // in-page wait. The caller's existing finally closes its browser context.
      new Promise((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(`Popup entry animations exceeded ${timeoutMs}ms.`),
            ),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
