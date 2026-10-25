import '@fontsource-variable/geist';
import '@radix-ui/themes/styles.css';
import './styles.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { createBrowserRouter } from 'react-router';
import { Api } from './api/client';
import { App, routes } from './app';

const root = document.getElementById('root');
if (root === null) {
  throw new Error('index.html has no element with the id root');
}

createRoot(root).render(
  <StrictMode>
    <App api={new Api({ baseUrl: window.location.origin })} router={createBrowserRouter(routes)} />
  </StrictMode>,
);
