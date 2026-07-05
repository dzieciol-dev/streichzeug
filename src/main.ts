import { mount } from "svelte";
import App from "./App.svelte";
import MarkerTextDemo from "./MarkerTextDemo.svelte";
import Widget from "./Widget.svelte";
import "./styles.css";

// Querystring-Schalter: `?demo=marker` mountet den Marker-Demo-Harness,
// `?widget=1` den Inhalt des schwebenden Widget-Fensters (eigenes
// Tauri-Window, siehe src-tauri/src/widget.rs). App.svelte bleibt der
// Default für das Main-Window.
const params = new URLSearchParams(window.location.search);
const Component =
  params.get("demo") === "marker"
    ? MarkerTextDemo
    : params.get("widget") === "1"
      ? Widget
      : App;

// Svelte-5-Mount-API. `new Component(...)` ist zur Laufzeit nicht mehr gültig
// (component_api_invalid_new); Legacy-Autoring der Komponenten (export let,
// createEventDispatcher) bleibt davon unberührt.
const app = mount(Component, {
  target: document.getElementById("app")!,
});

export default app;
