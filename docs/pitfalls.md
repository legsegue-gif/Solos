# Pitfalls

Mistakes that were made here, one per line:
what goes wrong, where it was seen, and how to check for it. Read before
touching the area; add a line when a new one is found.

## Process

| Pitfall | Seen in | Check |
|---|---|---|
| Copying the reference app's prose (prompts, tag names) instead of following its behaviour. | This project: summary instructions, attachment tag | Behaviour and numbers may follow the reference app, with a note of where they came from; text sent to models is written here. |
| Searching terminal output for "\nX" in Swift: "\r\n" is one Character, so the search never matches and a working terminal looks stuck. | This project, terminal probe (a "stalled" 40 KB paste that had finished in 0.1 s) | Search for "\r\nX"; print the full output tail on failure, never a truncated one. |
| Upstream iSH syncs and notes carry the reference app's name (repo paths, issue references, env vars, guest paths); one pushed sync leaked it. | This project, deps/ish | Rename to the Solos forms before committing; `tools/namecheck` (the hooks) refuses the name in contents, paths, messages and identities. |

## Model-facing text

| Pitfall | Seen in | Check |
|---|---|---|
| Explanations hard-coded into tool results or return values. | Handoff rule | Results carry facts (exit code, sizes, paths); wording for people lives in the app. |

## Chat screen

| Pitfall | Seen in | Check |
|---|---|---|
| Lazy stacks go blank on very long replies after opening or any change at the bottom. | This project | Open a ~300k-character chat, stream into it, scroll up and back; nothing blank. |
| Replacing one chat with another at the same stack position reuses the old view's state. | This project, "New chat" from the menu | Open a chat, choose New chat from its menu, then again: each is empty. |
| Emptying the draft while the input method still holds text (a pinyin candidate, dictation, an autocorrection) lets it write the sent message back into the field; the keyboard stays up while the reply streams. | This project (device, Chinese keyboard) | Send drops focus and resigns first responder before clearing (soft keyboard only, as the reference app's `performSend`); the field is rebuilt per send. Send with a word still underlined: field empty, keyboard down. |
| A horizontal `ScrollView` against the navigation bar (a path bar) is given the bar's height as a top inset from the second opening of a sheet on, and draws its content out of sight; its geometry reads the same both times. | This project, file browser path bar | No scroll view there; open the sheet three times in a row and look. |
| Reading which row is at the top from `ScrollPosition.viewID` while the view is scrolled by edge: it stays empty, and whatever depends on it falls back to a default. | This project, chat step buttons | Record the rows' positions and compare with the visible top; press previous/next through a five-question chat and see each question land. |
| Two identical tool calls in one reply each run (writing a file twice, sending twice). | This project: qwen repeats calls | Byte-identical calls in one message run once; the others get a note. |

## Model turns

| Pitfall | Seen in | Check |
|---|---|---|
| Prompt text that invites the model to write a chain of calls in one reply ("calls run in the order you write them"): it wrote 70 calls without seeing any result, and all were run. | This project, 2026-09-28 | Count tool calls per assistant message in a real turn (the request log: one request, many calls). |
| A fixed total timeout on a download cuts a large file mid-body and reports it as a decoding error; the model then retries elsewhere. | This project, `fetch`, 12.8 MB image | Time out on silence, not on size; the error says how many bytes arrived. |

## Settings and context

| Pitfall | Seen in | Check |
|---|---|---|
| Initial state set in `onAppear` is reset on coming back from a pushed page, undoing what was chosen there. | This project, endpoint editor's default model | Set it once in `init`; choose on the pushed page, go back, the choice holds, Save keeps it. |

## Sandbox and platform

| Pitfall | Seen in | Check |
|---|---|---|
| A bind mount records only its own path in fakefs, so `mkdir -p` under it fails. | This project, `/solos/ws/x` | `record_guest_parents`; `mkdir -p /solos/ws/a/b` on a fresh install. |
| A background command with its output not redirected keeps the call open until it exits. | Measured in this guest | Prompt note; `(sleep 20; …) &` returns after 20 s, `… >/dev/null 2>&1 &` at once. |
| Changing guest files from the host outside the workspace (fakefs keeps inode records in `meta.db`) leaves the guest's view inconsistent. | This project, sandbox and file browser | The app writes only under `/solos/ws`; `FileBrowserTests.testOnlyTheWorkspaceCanBeChanged`. |
| The guest's `hostname` is the host's (on a simulator, the Mac's name); a `\h` prompt shows it. | This project, measured | The terminal profile sets its own prompt; check `echo $PS1` in the terminal. |
| A terminal's first output (the prompt) can arrive before the spawn call returns its id, and is lost if output is routed by id only. | This project | `output_before_the_terminal_id_is_known_reaches_that_terminal`; the probe's `terminal_prompt` check. |
| Terminal input written without handling a full line buffer drops the rest of a paste. | This project (input side) | Input retries on `-EAGAIN`; the probe's `terminal_paste_40000` check. |
| A notification's completion handler called off the main thread crashes. | This project | Deliver the handler on the main queue; tap a banner while the app is in the background. |
| Copying a config path without checking the program reads it: pip in this guest reads `/etc/xdg/pip/pip.conf` and `/etc/pip.conf`, not the reference app's `/etc/pip/pip.conf`, so a pip mirror there was silently unused. | This project, package mirrors | Run the tool verbosely after switching and see which address it fetched from (`pip download -v`, `apk update`). |
| A kernel upgrade that needs the host to do something new, taken without doing it: iSH's footprint memory governor (upstream `6d1178f3`) expects the app to feed it the footprint and allowance every 250 ms; unfed, V8's startup commitment hits the fixed ledger cap and node waits three minutes and runs nothing. In the simulator `os_proc_available_memory()` answers 0. | This project, iSH sync to `e1d57948` (found by bisecting against the reference app's older kernel) | After syncing iSH, read what its own app changed alongside (`app/AppDelegate.m`) and any new host-facing setters in the kernel headers (the previous sync skipped the governor wiring as "app only") and run `node -e "process.exit(3)"` in the probe: 3, in seconds. |
| iSH starts every `node` with `--jitless` (no WebAssembly) and `--require`s `/lib/wasm-polyfill.js` and `/lib/fetch-polyfill.js` only if they exist; iSH's app overlays them from `app/RootfsPatch.bundle`, a plain Alpine rootfs has neither, so `fetch` throws "WebAssembly is not defined" and npm MCP servers that use it die at start. | This project, 12306-mcp in the probe | The iSH sandbox writes both at boot (`NODE_POLYFILLS`); after an iSH sync, diff `app/RootfsPatch.bundle` too, and run `node -e "fetch('https://example.com').then(r=>console.log(r.status))"` in the probe: 200. |
| iSH's futex "safety valve" ended any process with one thread in an untimed futex wait for 180 s and no live children, with exit code 0; Node parks its idle workers that way, so every Node program died silently at ~183 s (a cold `npx` install mid-way, leaving a half-written `~/.npm` that then failed with `EEXIST`/`ENOTEMPTY`; an idle MCP server). The reference app's kernel has the same valve. | This project, 12306-mcp (found with a 250 s `setTimeout` that never fired) | deps/ish ends the process only when every thread is in an untimed futex wait. In the probe: `node -e "setTimeout(()=>console.log('done'),250000)"` prints `done`; `node -e "process.exit(3)"` returns 3 at once. |
| The probe blocks for good in `print` when one line is too big for the console pipe (a progress bar's output); the process looks hung. | This project, `SOLOS_PROBE_SHELL` | Print the tail of long output only. |
| CLLocationManager created on a thread without a run loop never calls its delegate; every request times out. | This project (the Probe opens the core off the main actor) | Create and drive the manager on main (`LocationProvider`). |
| A large paste into an interactive `/bin/sh` on macOS (bash 3.2 with its line editor) loses bytes when the machine is busy: lines merge, a heredoc's end marker is never seen (10 of 64 runs of a 40 KB heredoc under load). bash without line editing, dash and zsh lost none; writing to our own pty with backpressure did not help. The host terminal test also timed out on CI with a 5 s wait (7–8 s under load). | This project: the host terminal test, CI | The host terminal runs `/bin/zsh -f` on macOS (`sandbox/host.rs`); the test waits up to a minute. To check: 32 copies of the test binary at once, several rounds. |
