import { mount } from 'svelte';
import './app.css';
import DatabasePage from './lib/DatabasePage.svelte';

mount(DatabasePage, { target: document.getElementById('app') });
