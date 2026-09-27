# DEPRECATED — legacy frontend

This directory is the **old** frontend. It is not built and not served.

`src-tauri/tauri.conf.json` points `devUrl` at `http://localhost:5173` and
`frontendDist` at `../ui/dist`, and the root `tauri:dev` script runs
`npm --prefix ui run dev`. Only [`../ui`](../ui) is shipped.

The `@shared` alias that used to point here (`@shared` -> `../src`) has been
removed from `ui/vite.config.ts`: nothing under `ui/src` imported it, and the
alias let this tree leak back into the shipped bundle.

Use `ui/` for all work. Port anything still worth keeping across by hand —
`ui/src/lib/ipc.ts` is the current IPC surface and is the closer of the two
IPC implementations. Nothing here is type-checked, built, or tested, so it can
rot silently; treat it as a reference, not a dependency.
