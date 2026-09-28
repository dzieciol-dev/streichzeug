// Prüft, dass jedes @tauri-apps/*-npm-Paket dieselbe Major.Minor-Version hat
// wie die zugehörige Rust-Crate in src-tauri/Cargo.lock. `tauri build` bricht
// sonst mit „version mismatched Tauri packages" ab — die normale CI baut aber
// kein Bundle und merkt das erst beim Release (v0.6.2, Build-Abbruch).
//
// Aufruf: node scripts/check-tauri-versions.mjs
import { readFileSync } from "node:fs";

const lock = JSON.parse(readFileSync("package-lock.json", "utf8")).packages;
const cargoLock = readFileSync("src-tauri/Cargo.lock", "utf8");

const crateVersions = new Map();
for (const m of cargoLock.matchAll(/^name = "([^"]+)"\nversion = "([^"]+)"/gm)) {
  crateVersions.set(m[1], m[2]);
}

const majorMinor = (v) => v.split(".").slice(0, 2).join(".");
const mismatches = [];

for (const [path, info] of Object.entries(lock)) {
  const m = path.match(/^node_modules\/@tauri-apps\/(api|plugin-[a-z-]+)$/);
  if (!m) continue;
  const crate = m[1] === "api" ? "tauri" : `tauri-${m[1]}`;
  const crateVersion = crateVersions.get(crate);
  if (!crateVersion) continue; // npm-Paket ohne Rust-Gegenstück
  const ok = majorMinor(crateVersion) === majorMinor(info.version);
  console.log(`${ok ? "ok      " : "MISMATCH"} ${crate} ${crateVersion} ↔ @tauri-apps/${m[1]} ${info.version}`);
  if (!ok) mismatches.push(crate);
}

if (mismatches.length) {
  console.error(`\n${mismatches.length} Tauri-Paket(e) mit abweichender Major.Minor-Version.`);
  process.exit(1);
}
