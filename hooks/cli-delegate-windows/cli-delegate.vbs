' Launches cli-delegate.ps1 with no console window flash.
' wscript.exe is a GUI app, and Run with window style 0 hides the child.
Dim shell, fso, scriptDir, ps1, uri
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
ps1 = scriptDir & "\cli-delegate.ps1"
uri = ""
If WScript.Arguments.Count > 0 Then uri = WScript.Arguments(0)
shell.Run "powershell.exe -NoProfile -ExecutionPolicy Bypass -File """ & ps1 & """ -Uri """ & uri & """", 0, False
