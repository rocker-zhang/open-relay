param(
    [string]$Uri,
    [switch]$Visible,
    [switch]$DryRun
)

# cli-delegate://<cli>?args='<argument string>'[&showWindow=true]
# Example: cli-delegate://oly?args='send xxx --node xxx key:enter'
#   -> runs: oly send xxx --node xxx key:enter
# Runs hidden by default (output appended to cli-delegate.log next to this
# script). Add &showWindow=true to the URL to pop a visible console instead.

# Allowed CLIs: add entries here to support more tools.
#   '<name in URL>' = '<executable name or full path>'
$AllowedClis = @{
    oly = 'oly'
}

$LogFile = Join-Path $PSScriptRoot 'cli-delegate.log'

function Write-Log([string]$Message) {
    "[{0}] {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Message |
        Out-File -FilePath $LogFile -Append -Encoding utf8
}

if ([string]::IsNullOrWhiteSpace($Uri)) {
    Write-Host "Usage: cli-delegate://<cli>?args='<args>'[&showWindow=true]"
    Write-Host "Allowed CLIs: $($AllowedClis.Keys -join ', ')"
    exit 1
}

$parsed = [System.Uri]$Uri
$cliName = $parsed.Host.ToLower()

# Parse and URL-decode all query parameters.
$queryParams = @{}
if ($parsed.Query -and $parsed.Query.Length -gt 1) {
    foreach ($pair in $parsed.Query.TrimStart('?') -split '&') {
        $kv = $pair -split '=', 2
        $key = [System.Uri]::UnescapeDataString($kv[0])
        $queryParams[$key] = if ($kv.Count -gt 1) { [System.Uri]::UnescapeDataString($kv[1]) } else { '' }
    }
}

# Strip one pair of surrounding quotes from args if present ('...' or "...").
$argString = ([string]$queryParams['args']).Trim()
if ($argString.Length -ge 2 -and (
    ($argString.StartsWith("'") -and $argString.EndsWith("'")) -or
    ($argString.StartsWith('"') -and $argString.EndsWith('"')))) {
    $argString = $argString.Substring(1, $argString.Length - 2)
}

if (-not $AllowedClis.ContainsKey($cliName)) {
    $msg = "Error: '$cliName' is not in the allowed CLI list."
    Write-Host $msg
    Write-Log "$msg URI=$Uri"
    exit 1
}

$exe = $AllowedClis[$cliName]

# showWindow=true: relaunch this script in a visible console and exit.
if ($queryParams['showWindow'] -eq 'true' -and -not $Visible -and -not $DryRun) {
    Start-Process powershell.exe -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', "`"$PSCommandPath`"",
        '-Uri', "`"$Uri`"", '-Visible')
    exit 0
}

if ($DryRun) {
    Write-Host "CLI  : $exe"
    Write-Host "Args : $argString"
    Write-Host "(dry run - not executed)"
    Read-Host "`nPress Enter to close"
    exit 0
}

Write-Log "RUN $exe $argString (URI=$Uri)"

if ($Visible) {
    $startParams = @{ FilePath = $exe; NoNewWindow = $true; Wait = $true }
    if ($argString) { $startParams.ArgumentList = $argString }
    try {
        Start-Process @startParams
    } catch {
        Write-Log "ERROR: $_"
        Write-Host "Error: $_"
    }
} else {
    $tempOut = [IO.Path]::GetTempFileName()
    $tempErr = [IO.Path]::GetTempFileName()
    $hiddenParams = @{ FilePath = $exe; Wait = $true; WindowStyle = 'Hidden'; PassThru = $true
        RedirectStandardOutput = $tempOut; RedirectStandardError = $tempErr }
    if ($argString) { $hiddenParams.ArgumentList = $argString }
    try {
        $proc = Start-Process @hiddenParams
        $stdout = Get-Content $tempOut -Raw
        $stderr = Get-Content $tempErr -Raw
        Write-Log "EXIT $($proc.ExitCode)"
        if ($stdout) { Write-Log "OUT: $($stdout.Trim())" }
        if ($stderr) { Write-Log "ERR: $($stderr.Trim())" }
    } catch {
        Write-Log "ERROR: $_"
    }
    Remove-Item $tempOut, $tempErr -ErrorAction SilentlyContinue
}
