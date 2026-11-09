# Third-party components and notices

GlassSuite itself is licensed GPL-3.0 (see `LICENSE`). This file tracks third-party components that
are redistributed with, or linked into, the project.

| Component | Version | License | Redistributed? |
|---|---|---|---|
| React / React DOM | 19.x | MIT | bundled into `dist/*` |
| Vite | 6.x | MIT | build-time only |
| @vitejs/plugin-react | 4.x | MIT | build-time only |
| TypeScript | 5.x | Apache-2.0 | build-time only |
| Tauri | 2.x | MIT / Apache-2.0 | linked (added in a later milestone) |
| `windows` crate | 0.62 | MIT / Apache-2.0 | linked |

## Fonts

No font files are currently redistributed. If a bundled font is added later, its license text
**must** ship next to it (for example LXGW WenKai is OFL-1.1 — the full OFL text goes into
`assets/fonts/OFL.txt`, not just a link).

## Application icons

Application icons are **not** redistributed. They are read at runtime from the local system through
Windows shell APIs (`SHGetFileInfo` / `IShellItemImageFactory`), so no third-party icon assets and no
application trademarks are packaged with this project.

## Microsoft APIs

GlassSuite calls documented Win32 / WinRT APIs (DWM, Shell, Media Control, Notifications). Use of
these APIs is subject to the Windows SDK terms; no Microsoft code is redistributed.
