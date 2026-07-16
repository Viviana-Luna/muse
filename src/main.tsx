import React from 'react';
import { createRoot } from 'react-dom/client';
import { App } from '@/views';
import '@/assets/styles/app.css';
import '@/assets/styles/feedback.css';
import '@/assets/styles/story-states.css';
import '@/assets/styles/persona-library.css';
import '@/assets/styles/app-shell.css';
import '@/assets/styles/liquid-glass.css';
import '@/assets/styles/sessions-page.css';
import '@/assets/styles/typography.css';

createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
