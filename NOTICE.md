# Third-party components and notices

GlassSuite itself is licensed GPL-3.0 (see `LICENSE`). This file tracks third-party components that
are redistributed with, or linked into, the project.

| Component | Version | License | Redistributed? |
|---|---|---|---|
| React / React DOM | 19.x | MIT | bundled into `dist/*` |
| Vite | 6.x | MIT | build-time only |
| @vitejs/plugin-react | 4.x | MIT | build-time only |
| TypeScript | 5.x | Apache-2.0 | build-time only |
| Tauri | 2.x | MIT / Apache-2.0 | linked |
| `windows` crate | 0.62 | MIT / Apache-2.0 | linked |
| `rusqlite` (bundled SQLite) | 0.32 | MIT / SQLite public domain | linked (notification WAL snapshot) |
| `ureq`, `base64`, `sysinfo`, `windows-future` | — | MIT / Apache-2.0 | linked |

## Fonts

**No font files are currently redistributed.** The UI uses system font families only. The two ~19 MB
font files from the predecessor project were intentionally not imported (38,068,020 bytes saved).

`licenses/OFL-1.1.txt` contains the complete SIL Open Font License 1.1 text for **future** use: if a
bundled/subsetted font is ever added, its copyright notice plus this license text must ship with it,
and the font must be registered in this table. Today the OFL text applies to no file in this repo.

## Application icons

Application icons are **not** redistributed. They are read at runtime from the local system through
Windows shell APIs (`SHGetFileInfo` / `IShellItemImageFactory`), so no third-party icon assets and no
application trademarks are packaged with this project.

## Microsoft APIs

GlassSuite calls documented Win32 / WinRT APIs (DWM, Shell, Media Control, Notifications). Use of
these APIs is subject to the Windows SDK terms; no Microsoft code is redistributed.
