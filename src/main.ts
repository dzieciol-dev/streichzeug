import { mount } from "svelte";
import App from "./App.svelte";
import MarkerTextDemo from "./MarkerTextDemo.svelte";
import StageImageDemo from "./StageImageDemo.svelte";
import Widget from "./Widget.svelte";
import "./styles.css";

// Querystring-Schalter: `?demo=marker` mountet den Marker-Demo-Harness,
// `?demo=stage-image` die Bild-Bühnen-Demo (Stufe 3), `?widget=1` den
// Inhalt des schwebenden Widget-Fensters (eigenes Tauri-Window, siehe
// src-tauri/src/widget.rs). App.svelte bleibt der Default fürs Main-Window.
const params = new URLSearchParams(window.location.search);
const Component =
  params.get("demo") === "marker"
    ? MarkerTextDemo
    : params.get("demo") === "stage-image"
      ? StageImageDemo
      : params.get("widget") === "1"
        ? Widget
        : App;

// Globaler Drop-Guard: Ohne `preventDefault` NAVIGIERT die WebView beim
// Datei-Drop zur Datei — eine gedroppte PDF ersetzt dann die komplette
// App-UI ohne Weg zurück (Beta-Befund 2026-07-05). Die Komponenten-Handler
// (App.svelte) laufen in der Bubble-Phase zuerst und verarbeiten Bilder/
// Text; dieser Fallback fängt alles Unbehandelte ab — auch Drops auf
// Onboarding-Wizard, Widget-Fenster und Demo-Harnesses, die keine eigenen
// Handler haben.
window.addEventListener("dragover", (e) => {
  if (e.dataTransfer?.types.includes("Files")) {
    e.preventDefault();
  }
});
window.addEventListener("drop", (e) => {
  if (e.dataTransfer?.types.includes("Files")) {
    e.preventDefault();
  }
});

// Svelte-5-Mount-API. `new Component(...)` ist zur Laufzeit nicht mehr gültig
// (component_api_invalid_new); Legacy-Autoring der Komponenten (export let,
// createEventDispatcher) bleibt davon unberührt.
const app = mount(Component, {
  target: document.getElementById("app")!,
});

export default app;
