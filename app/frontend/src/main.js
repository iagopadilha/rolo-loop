/* Front entry point: "/" = project hub; "/p/<id>/" = project panel. Same index.html for both. */
import './util/theme.js'; // applies the saved theme (light | dark | system) and follows the OS while on "system"
import { initDesktop } from './util/desktop.js'; // window chrome when the page runs inside the desktop app (inert in a browser)
import { projectIdFromUrl } from './api/api_client.js';
import { startHub } from './pages/hub.js';
import { startPanel } from './pages/project_panel.js';

function start() {
  initDesktop();
  const root = document.getElementById('root');
  const id = projectIdFromUrl(location.pathname);
  if (id) {
    // without the trailing slash relative URLs would break; normalize to /p/<id>/
    if (!location.pathname.endsWith('/')) history.replaceState(null, '', location.pathname + '/' + location.search + location.hash);
    document.body.classList.add('page-panel');
    startPanel(root, id);
  } else {
    document.body.classList.add('page-hub');
    startHub(root);
  }
}

start();
