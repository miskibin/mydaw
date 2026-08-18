import { render } from 'solid-js/web';
import { App } from './App';
import './styles/theme.css';
import './styles/app.css';

const root = document.getElementById('root');
if (!root) throw new Error('#root missing from index.html');

render(() => <App />, root);
