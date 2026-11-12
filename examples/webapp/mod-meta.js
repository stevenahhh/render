import { value } from './mod-a.js';
const url = import.meta.url;
mark('MOD import.meta.url path:' + url.slice(url.lastIndexOf('/webapp/')) + ' value:' + value + ' evaluatedOnce:' + (globalThis.__modAEvals === 1));
