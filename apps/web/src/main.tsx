import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from '@/app';
import { openDraftStore } from '@/artifacts/draft-store';
import { createPageDrafts } from '@/artifacts/page-drafts';
import './style.css';

// Before anything renders, so the page holds its draft owner id whether or
// not a server is connected (decision 0018).
const drafts = createPageDrafts(openDraftStore());

const root = document.getElementById('root');
if (root === null) throw new Error('Missing application root.');
createRoot(root).render(
  <StrictMode>
    <App drafts={drafts} />
  </StrictMode>,
);
