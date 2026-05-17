# NER-Modelle (Layer 3)

Lokales ONNX-Modell für Layer-3-NER plus ONNX-Runtime-Library. Nichts
hier wird ins Git eingecheckt — Modell wird per Script bezogen, das
Manifest auch.

## Erwartete Dateien

| Datei | Quelle | Größe |
|---|---|---|
| `model.onnx` | Xenova/distilbert-base-multilingual-cased-ner-hrl, INT8 | ~129 MB |
| `tokenizer.json` | Xenova/distilbert-base-multilingual-cased-ner-hrl | ~3 MB |
| `MANIFEST.sha256` | wird vom Download-Script erzeugt, Format wie `sha256sum` | < 1 KB |
| `onnxruntime.dll` (Win) / `libonnxruntime.dylib` (Mac) | github.com/microsoft/onnxruntime release v1.22.0 (crate-required) | ~10 MB |

## Setup

```powershell
# Windows
powershell -ExecutionPolicy Bypass -File ../../scripts/download-ner-model.ps1
```

```bash
# macOS / Linux
bash ../../scripts/download-ner-model.sh
```

## Build aktivieren

```bash
cd ..               # nach src-tauri/
cargo build --features ner
```

Die ONNX-Runtime-Library muss zur Laufzeit neben der Binary liegen:

```powershell
Copy-Item models/onnxruntime.dll target/debug/    # Windows
```

```bash
cp models/libonnxruntime.dylib target/debug/      # macOS
```

## Settings

In `settings.json`:
```json
{ "enable_ner": true }
```

Oder Tray-Menü → „Erweiterte Erkennung" anhaken, App neu starten.
Default ist **off**, damit ein Slim-Build ohne Modell unverändert
nutzbar bleibt.

---

## Supply-Chain-Härtung — bewusste Stufung

Modell-Files sind binäre Artefakte aus externen Quellen. ONNX ist im
Gegensatz zu PyTorch-Pickles kein Code-Execution-Format (reines
Protobuf, vom ORT-Runtime memory-safe geparst), aber drei Risiken
bleiben grundsätzlich:

1. **Backdoor-Trigger im Modell** — bestimmter Input lässt PII unerkannt
   durch (silent failure)
2. **Konvertierungs-Drift** — wer das ONNX exportiert hat, könnte beim
   Export Gewichte manipuliert haben
3. **Stilles Ersetzen im Repo** — Modell wird über Jahre vom Account-
   Inhaber ausgetauscht, ohne dass wir's bemerken

Wir gehen das in drei Phasen an:

### Phase A — POC (jetzt)

- **Quelle:** Xenova auf HuggingFace. Etablierter Community-Maintainer
  (transformers.js), aber **kein „Verified"-Status** und kein
  Audit-Footprint
- **MANIFEST.sha256** wird beim Download erzeugt und zur Laufzeit
  geprüft — bei Mismatch (z. B. korrupter Download) verweigert der
  Loader den Start
- **Defense in Depth:** L1+L2 fängt strukturierte PII (Email, IBAN,
  Telefon, BIC, USt-ID, PLZ, Straße, URL, Datum) unabhängig vom Modell
  ab. Selbst ein backdoored Modell könnte diese Klassen nicht
  durchlassen

**Restrisiko:** beim Erst-Download vertrauen wir Xenova vollständig.
Akzeptabel für den POC, **nicht** für Release.

### Phase B — Vor erstem Release

- **Modell selbst hosten** (eigenes S3/CDN), Download-URL im Script
  zeigt darauf, nicht mehr HF
- **Hashes im Source pinnen:** `EXPECTED_HASHES`-Konstante in `ner.rs`,
  Loader prüft MANIFEST gegen Code-Konstante UND gegen tatsächliche
  Files. Modell-Update = Git-Commit nötig
- **Eigene Konvertierung:** Davlan-Original mit `optimum-cli export
  onnx` selbst nach ONNX bringen, statt Xenova-Mirror — eliminiert die
  Konvertierungs-Drift
- **Cargo-deny / cargo-audit** prüft per CI, dass keine RUSTSEC-IDs in
  ORT/tokenizers neu offen sind

### Phase C — Enterprise

- **Eigenes Fine-Tuning** auf curated DE-Daten (Mittelstand-Korrespondenz,
  Behörden-Schreiben, branchenspezifisches Vokabular)
- **Sigstore / cosign-signiertes Modell-Artefakt**, Loader prüft Signatur
- **Reproducible Builds inkl. Modell-Konvertierungs-Pipeline** —
  third-party auditierbar
- **Code-signed Distribution** der Binary

---

## MANIFEST.sha256 — Runtime-Check

Der NER-Loader prüft beim Start die SHA-256-Hashes der Modell-Files
gegen die Einträge in `MANIFEST.sha256`. Bei Mismatch verweigert er den
Load und loggt laut.

Format pro Zeile (sha256sum-kompatibel):
```
<sha256-hex>  <relative-path>
```
Zeilen mit `#` als erstes Zeichen sind Kommentare und werden ignoriert.

Manuelle Neuerzeugung nach Modell-Update:

```bash
cd src-tauri/models
sha256sum model.onnx tokenizer.json > MANIFEST.sha256
```

In Phase B wird zusätzlich ein **Sollwert** im Source committed
(`models/MANIFEST.sha256.expected` oder als Code-Konstante) — der
Runtime-Check vergleicht dann Manifest gegen Soll, nicht nur Manifest
gegen Files.

## Lizenz

- Davlan-Original-Modell: Apache-2.0 (siehe HF-Model-Card)
- Xenova-ONNX-Konvertierung: erbt Apache-2.0
- ONNX Runtime: MIT
- Alle drei kompatibel mit der Streichzeug-Lizenz (MIT OR Apache-2.0)
