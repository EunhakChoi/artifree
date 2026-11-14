import { StrictMode, useState } from 'react';
import { createRoot } from 'react-dom/client';
import './app.css';

function Counter({ label }: { label: string }) {
  const [count, setCount] = useState(0);
  return (
    <button type="button" onClick={() => setCount(count + 1)}>
      {label}: {count}
    </button>
  );
}

const root = document.getElementById('root');
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <Counter label="Clicks" />
    </StrictMode>,
  );
}
