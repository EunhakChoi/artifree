/** CSS validity and maintainability; Prettier owns formatting. */
export default {
  extends: ['stylelint-config-recommended'],
  ignoreFiles: [
    '**/dist/**',
    '**/node_modules/**',
    '.agents/**',
    '.claude/**',
    '.artifree/**',
  ],
  rules: {
    'at-rule-no-unknown': [
      true,
      {
        ignoreAtRules: [
          'theme',
          'custom-variant',
          'apply',
          'source',
          'utility',
          'variant',
          'plugin',
          'reference',
          'config',
        ],
      },
    ],
    'at-rule-prelude-no-invalid': [true, { ignoreAtRules: ['apply'] }],
    // Standalone document sections use disjoint DOM trees; this heuristic cannot infer that.
    'no-descending-specificity': null,
  },
};
