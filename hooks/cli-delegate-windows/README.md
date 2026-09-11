# cli-delegate

Windows URL-protocol helper for launching trusted command-line tools from `cli-delegate://` links.

## Install

1. Keep this folder anywhere you prefer.
2. Double-click `install-cli-delegate.cmd`.
3. Test with:

   ```text
   cli-delegate://oly?args='--help'&showWindow=true
   ```

The installer registers the protocol under `HKCU\Software\Classes\cli-delegate`, so no administrator prompt is needed.

## Uninstall

Run `uninstall-cli-delegate.cmd`.

## URL format

```text
cli-delegate://<cli>?args='<argument string>'[&showWindow=true]
```

- `<cli>` must be listed in `$AllowedClis` in `cli-delegate.ps1`.
- `args` is passed to that CLI.
- `showWindow=true` opens a visible console; omit it to run hidden.

Example:

```text
cli-delegate://oly?args='send "hello" --node abc key:enter'&showWindow=true
```

## Files

- `install-cli-delegate.cmd` registers the URL protocol.
- `uninstall-cli-delegate.cmd` removes it.
- `cli-delegate.reg` is a reference `.reg` example; use the installer instead because registry paths cannot be file-relative.
- `cli-delegate.vbs` launches PowerShell without flashing a console.
- `cli-delegate.ps1` parses the URL, enforces the allowed-CLI list, and runs the command.
- `oly-notify-hook.cmd` renders Oly notification events as markdown stickers with `rusticker`, including links that use this protocol.

Hidden runs append output and errors to `cli-delegate.log`.

## Security

Only CLIs explicitly listed in `$AllowedClis` can run. Add new tools deliberately, because any application can open a `cli-delegate://` link.
