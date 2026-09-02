# Clipsta v2.3.3

## Highlights

- **Automatic updates.** Clipsta now checks for new versions on launch and can install them in one click — no manual re-download. When an update is available you'll see an "Update available" prompt; choosing **Install & Restart** downloads the signed update, verifies it, and relaunches into the new version.
- **Smoother, hitch-free clip saving.** Saving a clip no longer stalls capture. This fixes the random slowdowns some users saw when saving several clips back-to-back.

## Fixes

- **Fixed double clips.** Triggering a save (via hotkey) could produce two clips at once when Clipsta was set to start at login. Only one clip is saved per trigger now.
- **Fixed save-time stutter.** The clip save path was briefly blocking the capture pipeline while copying video data. Saves now run without interrupting recording, so gameplay stays smooth even during long or consecutive saves.

## Under the hood

- **Crash logging.** If Clipsta or its capture engine ever crashes, details are now written to `%APPDATA%\Clipsta\logs\crash.log` to make problems easier to diagnose and fix.
- **Signed builds & update pipeline.** Release builds are now produced and signed through CI, laying the groundwork for verified auto-updates and code signing.

## Notes

- Updates are delivered automatically for installed apps going forward.
- No settings changes are required — existing hotkeys, output folder, and preferences carry over.
