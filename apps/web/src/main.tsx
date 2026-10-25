import { StrictMode, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { scaffoldDescription } from '@artifree/contracts';
import type { Layer } from '@artifree/contracts';
import './style.css';

const layers: Layer[] = ['presentation', 'logic', 'provider', 'shared'];

function App() {
  const [layer, setLayer] = useState<Layer>('presentation');
  const items = scaffoldDescription.components.filter(
    (item) => item.layer === layer,
  );
  return (
    <div className="workspace">
      <aside>
        <a className="brand" href="#">
          <span className="mark">a</span>artifree
          <span className="version">0.1</span>
        </a>
        <p className="eyebrow">WORKSPACE FOUNDATION</p>
        <nav aria-label="Architecture layers">
          {layers.map((item, index) => (
            <button
              key={item}
              aria-pressed={layer === item}
              onClick={() => setLayer(item)}
            >
              <span>0{index + 1}</span>
              {item}
            </button>
          ))}
        </nav>
        <p className="aside-note">
          Open artifacts.
          <br />
          Any agent.
        </p>
        <a className="repo" href="https://github.com/EunhakChoi/artifree">
          GitHub ↗
        </a>
      </aside>
      <main>
        <header>
          <span>Artifree / workspace</span>
          <span className="badge">Scaffolding</span>
        </header>
        <section className="hero">
          <p className="eyebrow">YOUR AGENTS. YOUR ARTIFACTS.</p>
          <h1>
            A place for what
            <br />
            your agents <em>make.</em>
          </h1>
          <p>
            The foundation for a workspace you can run on your own machine or
            server.
          </p>
        </section>
        <div className="notice" role="note">
          <strong>Development shell</strong>
          <span>
            This screen shows a build-time module inventory. Artifact creation,
            API connections, storage, and execution are planned.
          </span>
        </div>
        <section className="module-section" aria-label={`${layer} modules`}>
          <div className="section-title">
            <h2>{layer}</h2>
            <span>{items.length} modules</span>
          </div>
          <div className="cards">
            {items.map((item) => (
              <article key={item.id}>
                <div className="card-top">
                  <span className={`state ${item.status}`}>
                    {item.status === 'scaffold' ? 'Scaffold exists' : 'Planned'}
                  </span>
                  <span className="arrow">↗</span>
                </div>
                <h3>{item.name}</h3>
                <p>{item.summary}</p>
                <code>{item.path}</code>
              </article>
            ))}
          </div>
        </section>
        <footer>
          <span>Presentation · TypeScript</span>
          <span>Logic · Rust</span>
          <span>Server framework · Open decision</span>
        </footer>
      </main>
    </div>
  );
}

const root = document.getElementById('root');
if (root === null) throw new Error('Missing application root.');
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
