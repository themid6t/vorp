import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './theme.css';
import { mount } from 'svelte';
import App from './App.svelte';

const target = document.getElementById('app');
if (target) mount(App, { target });
