# Splitting the app into modules

The entry module may import other modules under `src/` by relative path.
A typical package looks like this:

```text
index.html
src/main.tsx
src/App.tsx
src/components/Counter.tsx
src/app.css
images/logo.svg
```

`src/main.tsx` renders the root component:

```tsx
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './app.css';

const root = document.getElementById('root');
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
```

A file outside `src/`, such as `images/logo.svg`, is copied into the build
as it is, at the same path. Refer to it from `index.html`, or from code as a
string relative to the page (`'./images/logo.svg'`), not through a CSS
`url()` or an `import`: both make it bundler input, which the build does not
take for images.
