# PromptForge

> A system-tray prompt enhancer. Press a global hotkey anywhere on your computer to turn rough prompts into precise ones — works in any app where you can select text.

Built with Tauri 2 + Rust + React + TypeScript.

---

## What it does

1. Select rough prompt text in any app (Notepad, Brave, VS Code chat, terminal, anywhere).
2. Press the global hotkey (default `Ctrl+Alt+E` on Windows, `Cmd+Option+E` on macOS).
3. PromptForge captures the selection, sends it to an LLM with a meta-prompt that rewrites it into a precise developer prompt, and pastes the enhanced version back — replacing your selection in place.
4. A small "Enhancing…" pill appears near the cursor while the API call is running.

Think Wispr Flow, but for written prompts to coding agents (Claude Code, Cursor, ChatGPT) instead of voice.

## v1 status

| Phase | Status |
|---|---|
| 1 — Tray icon + Quit | done |
| 2 — Global hotkey (`Ctrl+Alt+E`) | done |
| 3 — Clipboard capture / replace | done |
| 4 — LLM integration (Groq + Llama 3.3 70B) | done |
| 5 — Floating "Enhancing…" status pill | done |
| 6 — Settings window (API key, hotkey rebind, test connection) | done |
| 7 — Cross-platform polish, code-signing, auto-update | partial / future |

The core round-trip (select → hotkey → enhanced text pastes back) is working on Windows. macOS support exists in code but is untested.

## Apps confirmed working on Windows

- **Notepad** (Windows 11)
- **Brave** (address bar, page text)
- **VS Code chat panel** (BLACKBOX AI / Claude Code extensions)

## Apps known not to work / unconfirmed

The synthetic-input dispatch may be blocked by Electron-based apps that debounce or filter `WM_KEYDOWN` events. Running PromptForge as administrator can sometimes work around UIPI-related blocks. To be expanded with real-world data once more users test it.

---

## Setup

### Prerequisites

- **Windows 10/11** or **macOS 12+** (Linux deferred to v2)
- **Rust** stable toolchain — install via [rustup](https://www.rust-lang.org/tools/install)
- **Node.js 20+** and **npm**
- **Windows only:** Visual Studio Build Tools 2022 with the "Desktop development with C++" workload (rustup will offer to install this for you)
- A **Groq API key** (free, no credit card) — get one at [console.groq.com/keys](https://console.groq.com/keys)

### Install dependencies

```sh
git clone https://github.com/Lakshmiyadav65/PromptEnhancer.git
cd PromptEnhancer
npm install
```

### Provide your Groq API key

Create a `.env` file at the project root (or copy `.env.example`) and paste your key:

```
GROQ_API_KEY=gsk_your_key_here
```

The `.env` file is gitignored — your key never gets committed.

You can also set the key via the Settings window after launching the app — it gets stored in `settings.json` under the app's config directory (`%APPDATA%\com.promptforge.app\` on Windows). The env var takes precedence if both are set.

### Run in dev mode

```sh
npm run tauri dev
```

The first build is slow (~2 minutes — pulling and compiling Rust dependencies). Subsequent rebuilds are 5–30 seconds.

You should see:

- A small PromptForge tray icon appear in the system tray.
- Console output: `[env] loaded …/.env` and `[hotkey] registered: CommandOrControl+Alt+E`.

### Use it

1. Select some rough prompt text in any app (e.g., type `fix the dashboard` in Notepad and select it).
2. Press **`Ctrl+Alt+E`**.
3. A small "Enhancing…" pill appears near your cursor.
4. After 1–3 seconds, your selection is replaced with an enhanced version including `[CLARIFY: …]` markers asking for missing context.

### Build a release binary

```sh
npm run tauri build
```

Output:
- Windows: `src-tauri/target/release/promptforge.exe` plus an MSI installer in `src-tauri/target/release/bundle/msi/`.
- macOS: `.app` bundle and `.dmg` in `src-tauri/target/release/bundle/`.

The binary is **not code-signed** — Windows SmartScreen / macOS Gatekeeper will warn on first run. v1 ships unsigned by design; signing is deferred to v2.

---

## How it works (architecture)

```
src/
├── components/
│   ├── StatusIndicator.tsx + .css   ← Phase 5: floating "Enhancing…" pill
│   └── Settings.tsx + .css          ← Phase 6: API key + hotkey UI
└── App.tsx                          ← hash routing: #/status, #/settings, default

src-tauri/src/
├── main.rs                          ← entry point
├── lib.rs                           ← plugin registration + setup hook
├── tray.rs                          ← Phase 1: system tray icon + menu
├── hotkey.rs                        ← Phase 2: registers/re-registers global shortcut
├── clipboard.rs                     ← Phase 3: capture/replace via Win32 SendInput
├── enhance.rs                       ← Phase 4: Groq API call
├── status_window.rs                 ← Phase 5: position pill near cursor
└── settings.rs                      ← Phase 6: persist & expose Tauri commands

prompts/
└── enhancer-system-prompt.md        ← THE PRODUCT — meta-prompt that rewrites prompts
```

### The capture/replace pipeline

When the user presses the hotkey:

1. **Capture (`clipboard.rs::capture_selection`)** — the trickiest part on Windows:
   - Save the current clipboard contents.
   - Plant a unique sentinel string on the clipboard so we can later verify whether the synthetic Ctrl+C produced a real copy.
   - **Poll `GetAsyncKeyState(VK_CONTROL)` and `GetAsyncKeyState(VK_MENU)`** for the user's hotkey modifiers to release. Real users hold the hotkey for 250–450 ms after pressing, and a fixed-time settle delay isn't long enough.
   - Once both are released, call **`SendInput`** directly (the Windows path bypasses `enigo` because `Key::Unicode` produces `WM_CHAR`, which doesn't trigger the `Ctrl+C` shortcut).
   - Wait briefly for the OS clipboard to settle, then read.
   - If the clipboard still contains our sentinel, the copy didn't happen — we surface a clear error.
   - Restore the original clipboard contents.

2. **Show indicator (`status_window.rs`)** — show the "Enhancing…" pill near the cursor. Window is hidden until needed.

3. **Enhance (`enhance.rs`)** — POST to Groq's OpenAI-compatible endpoint with the meta-prompt as the system message and the captured text as the user message. 30s timeout.

4. **Replace (`clipboard.rs::replace_selection`)** — write the enhanced text to the clipboard and synthesize Ctrl+V.

5. **Hide indicator** — fired in a `finally`-style branch regardless of success or failure.

### Hard-won lessons

- **Synthetic Ctrl+C requires real virtual-key events on Windows.** `enigo`'s `Key::Unicode('c')` sends `WM_CHAR` (text input), not `WM_KEYDOWN` with `VK_C`. With Ctrl held, only the latter triggers a Copy shortcut. We bypass `enigo` on Windows and call `SendInput` directly.
- **Modifier-release polling is non-negotiable.** A fixed `tokio::time::sleep(250ms)` looks fine in isolated testing but breaks for many real users who hold their hotkey for 300–500ms while reading the screen.
- **Sentinel-based capture verification is essential.** Without it, a failed synthetic Ctrl+C silently returns the previous clipboard contents, making it look like capture worked when it didn't.

---

## Configuration

### Settings window

Open via the tray menu → **Settings**.

- **Groq API Key** — paste your key, click Save. Stored in the OS keychain.
- **Test Connection** — pings Groq with the active key (env or keychain). Reports round-trip latency.
- **Global Hotkey** — click the field, press your desired combo (e.g., `Ctrl+Shift+Space`), click Save. The new shortcut is registered immediately and persisted.

### Settings storage location

Both the API key and the hotkey are stored in:

- **Windows:** `%APPDATA%\com.promptforge.app\settings.json`
- **macOS:** `~/Library/Application Support/com.promptforge.app/settings.json`

The file is plain JSON, only readable by the current user (standard file ACLs). Same security model as `.env`.

The `.env` file overrides the saved API key when both are set — useful for development where you want the key in the project directory rather than the user-config directory.

---

## The meta-prompt

The actual *product* is `prompts/enhancer-system-prompt.md`. Everything else is plumbing.

The current version is a **placeholder** — derived from imagined examples, not from real developer prompts. It works, but the quality ceiling is set by this file. Once we have 5–10 real prompts that real users typed into Claude Code / Cursor, the meta-prompt should be rewritten using those as ground truth.

---

## Known limitations (v1)

- **Not code-signed.** Windows SmartScreen and macOS Gatekeeper will warn on first run. Click "More info" → "Run anyway" on Windows; on macOS, right-click the app and choose Open.
- **No auto-updates.** New versions require manual redownload.
- **No Linux support.**
- **macOS untested.** Code paths exist (cursor positioning, Cmd+Option+E modifier) but no integration testing yet.
- **Free Groq tier limits.** ~30 requests/minute on `llama-3.3-70b-versatile`.
- **Some Electron / WinUI apps may block synthetic input.** The pipeline surfaces a clear error message — you'll see *"synthetic Ctrl+C produced no copy"* in the dev console.
- **Apps with sandboxed renderers may block the synthetic Ctrl+V** even if Ctrl+C worked. Workaround: paste manually.

---

## Contributing

This is a personal project, but if you want to:

- **Try it and report which apps it works in** — open an issue with the app name + Windows version.
- **Improve the meta-prompt** — share rough prompts you typed into a coding agent recently; help us calibrate.
- **Port the macOS path** — `clipboard.rs` has a non-Windows branch that uses `enigo`. Will need real testing on Apple Silicon and Intel.

---

## License

MIT.
