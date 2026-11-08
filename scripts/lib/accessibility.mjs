import AxeBuilder from '@axe-core/playwright';

/** The WCAG levels the browser checks hold pages to. */
const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'];

/** Runs axe on `page` (a Playwright page or frame) and returns its violations. */
export async function accessibilityViolations(page) {
  const report = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  return report.violations;
}

/** Each violation's rule and the elements it names, for a failure message. */
export function summarizeViolations(violations) {
  return violations.map((violation) => ({
    id: violation.id,
    targets: violation.nodes.map((node) => node.target),
  }));
}
