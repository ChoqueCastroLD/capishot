# Windows packaging

See [installation policy checks](INSTALLATION.md) for the installer scope and path gates.

## Updates

The app reads `latest.json` from `https://capichan.com/capishot/desktop/latest.json` at launch and
every six hours (`CAPISHOT_UPDATE_URL` points it elsewhere; `off` turns it off). `package-linux.sh`
writes it:

```json
{
  "version": "0.13.9",
  "url_msi": "https://capichan.com/capishot/desktop/0.13.9/capishot-0.13.9-windows-x64.msi",
  "url_zip": "https://capichan.com/capishot/desktop/0.13.9/capishot-0.13.9-windows-x64-portable.zip",
  "sha256": {"msi": "…", "zip": "…"},
  "size": {"msi": 123, "zip": 456},
  "notes": "…",
  "notes_url": "https://…",
  "min_version": "0.13.0"
}
```

`notes` comes from `RELEASE_NOTES`, `notes_url` from `NOTES_URL` and `min_version` from
`MIN_VERSION` (older versions must update: no "Skip This Version"). Every field but `version` and
the URLs is optional, so older manifests still work.

On Windows the notice's **Update** downloads the file in the background into
`%LOCALAPPDATA%\Capishot\updates` (a portable copy: `CapishotData\updates`), resuming with Range
requests, and refuses it unless its SHA-256 matches `sha256`. **Restart and Update** quits the usual
way (unsaved documents ask first); then `capishot.exe --apply-update …` (a detached helper) waits
for the app to exit and runs `msiexec /i <msi> /passive /norestart` for an installed copy (Windows
asks for administrator rights), or copies the extracted ZIP over a portable copy's folder, keeping
`CapishotData`. It starts Capishot again; a failed install or a declined administrator prompt is
reported at the next launch. Preferences › Interface › Install Updates Automatically downloads at
once and installs when the app closes. Help › Check for Updates checks now.

## DirectML runtime

`package-linux.sh` puts `onnxruntime.dll` (NuGet `Microsoft.ML.OnnxRuntime.DirectML`, pinned
version and SHA-256) and `DirectML.dll` (`Microsoft.AI.DirectML`) beside `capishot.exe` and
`capishot-cli.exe` in both packages, with their licences (`licenses/` in the ZIP, the install
folder for the MSI). `--no-directml` leaves them out. `package.ps1` (WiX v5 on Windows) doesn't
bundle them yet.
