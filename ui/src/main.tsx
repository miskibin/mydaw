import { render } from 'solid-js/web';
import { App } from './App';
// Geist and Geist Mono, bundled rather than fetched. The app has to work with no
// network -- and inside Tauri a Google Fonts <link> is a CSP hole for the sake
// of a typeface.
import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './styles/wave.css';

const root = document.getElementById('root');
if (!root) throw new Error('#root missing from index.html');

render(() => <App />, root);
