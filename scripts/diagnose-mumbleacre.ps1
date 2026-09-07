[CmdletBinding()]
param(
    [string]$PackagePath,
    [string]$MumblePath,
    [string]$PluginLogPath,
    [string]$MurmurIni,
    [ValidateRange(10, 500)][int]$LogLines = 80
)

$ErrorActionPreference = "Stop"

function Write-Section([string]$Title) {
    Write-Output ""
    Write-Output "== $Title =="
}

function Write-Status([string]$Name, [string]$Value) {
    Write-Output ("{0}: {1}" -f $Name, $Value)
}

function First-ExistingPath([string[]]$Candidates) {
    foreach ($candidate in $Candidates) {
        if (![string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    return $null
}

$scriptDirectory = Split-Path -Parent $PSCommandPath
if ([string]::IsNullOrWhiteSpace($PackagePath)) {
    $PackagePath = First-ExistingPath @(
        (Join-Path $scriptDirectory "MumbleACRE.mumble_plugin"),
        (Join-Path (Split-Path -Parent $scriptDirectory) "dist\windows-x64\MumbleACRE.mumble_plugin")
    )
}
if ([string]::IsNullOrWhiteSpace($PluginLogPath)) {
    $localAppData = if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        [System.IO.Path]::GetTempPath()
    } else {
        $env:LOCALAPPDATA
    }
    $PluginLogPath = Join-Path $localAppData "MumbleACRE\logs\plugin.log"
}
if ([string]::IsNullOrWhiteSpace($MumblePath)) {
    $mumbleCandidates = @()
    if (![string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $mumbleCandidates += Join-Path $env:ProgramFiles "Mumble\client\mumble.exe"
    }
    $MumblePath = First-ExistingPath $mumbleCandidates
}

Write-Output "MumbleACRE diagnostic report (read-only)"
Write-Status "Generated" (Get-Date -Format "yyyy-MM-dd HH:mm:ss K")

Write-Section "Package"
if ([string]::IsNullOrWhiteSpace($PackagePath) -or !(Test-Path -LiteralPath $PackagePath)) {
    Write-Status "Bundle" "NOT FOUND"
} else {
    Write-Status "Bundle" $PackagePath
    try {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [System.IO.Compression.ZipFile]::OpenRead($PackagePath)
        try {
            $manifestEntry = $archive.GetEntry("manifest.xml")
            if ($null -eq $manifestEntry) {
                Write-Status "Manifest" "MISSING"
            } else {
                $reader = New-Object System.IO.StreamReader($manifestEntry.Open())
                try {
                    [xml]$manifest = $reader.ReadToEnd()
                    Write-Status "Plugin" $manifest.SelectSingleNode("/bundle/name").InnerText
                    Write-Status "Version" $manifest.SelectSingleNode("/bundle/version").InnerText
                } finally {
                    $reader.Dispose()
                }
            }
            Write-Status "DLL in bundle" ([bool]($null -ne $archive.GetEntry("mumbleacre_plugin.dll")))
        } finally {
            $archive.Dispose()
        }
    } catch {
        Write-Status "Bundle read" ("ERROR: " + $_.Exception.Message)
    }

    $distributionDirectory = Split-Path -Parent $PackagePath
    $dllPath = Join-Path $distributionDirectory "mumbleacre_plugin.dll"
    $checksumsPath = Join-Path $distributionDirectory "SHA256SUMS"
    if ((Test-Path -LiteralPath $dllPath) -and (Test-Path -LiteralPath $checksumsPath)) {
        $checksumLine = Get-Content -LiteralPath $checksumsPath | Where-Object {
            $_ -match '\s+mumbleacre_plugin\.dll$'
        } | Select-Object -First 1
        if ($null -ne $checksumLine) {
            $expected = ($checksumLine -split '\s+')[0]
            $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $dllPath).Hash.ToLowerInvariant()
            $hashStatus = if ($actual -eq $expected) { "OK" } else { "MISMATCH" }
            Write-Status "DLL SHA256" $hashStatus
        }
    }
}

Write-Section "Mumble"
if ([string]::IsNullOrWhiteSpace($MumblePath) -or !(Test-Path -LiteralPath $MumblePath)) {
    Write-Status "Executable" "NOT FOUND (pass -MumblePath to check a custom install)"
} else {
    $mumble = Get-Item -LiteralPath $MumblePath
    Write-Status "Executable" $mumble.FullName
    Write-Status "File version" $mumble.VersionInfo.FileVersion
}
$mumbleProcesses = @(Get-Process -Name mumble -ErrorAction SilentlyContinue)
if ($mumbleProcesses.Count -eq 0) {
    Write-Status "Running" "No"
} else {
    Write-Status "Running" ("Yes (PID " + ($mumbleProcesses.Id -join ", ") + ")")
}
$activeRunPrefixes = @($mumbleProcesses | ForEach-Object { "run_id=$($_.Id)-" })

Write-Section "Plugin log"
Write-Status "Path" $PluginLogPath
if (!(Test-Path -LiteralPath $PluginLogPath)) {
    Write-Status "Log" "NOT FOUND (start Mumble with the plugin once)"
} else {
    try {
        $recent = Get-Content -LiteralPath $PluginLogPath -Tail $LogLines
        $activeRun = if ($activeRunPrefixes.Count -eq 0) {
            $recent
        } else {
            @($recent | Where-Object {
                $line = $_
                $activeRunPrefixes | Where-Object { $line.Contains($_) }
            })
        }
        if ($activeRun.Count -eq 0) {
            $activeRun = $recent
        }
        $lastPipe = $activeRun | Where-Object {
            $_ -match "event=acre_pipe_(connected|disconnected|error)"
        } | Select-Object -Last 1
        if ($null -eq $lastPipe) {
            Write-Status "ACRE pipe" "No recent pipe event"
        } elseif ($lastPipe -match "event=acre_pipe_connected") {
            Write-Status "ACRE pipe" "CONNECTED"
        } elseif ($lastPipe -match "event=acre_pipe_disconnected") {
            Write-Status "ACRE pipe" "DISCONNECTED"
        } else {
            Write-Status "ACRE pipe" "ERROR (see log line below)"
        }
        $lastSoundWarning = $activeRun | Where-Object {
            $_ -match "event=acre_sound_(playback|load)_failed"
        } | Select-Object -Last 1
        if ($null -ne $lastSoundWarning) {
            Write-Status "Latest sound warning" $lastSoundWarning
        }
        Write-Output ""
        Write-Output ("Last {0} log lines (current Mumble run when available):" -f $LogLines)
        $activeRun
    } catch {
        Write-Status "Log" ("UNREADABLE: " + $_.Exception.Message)
    }
}

Write-Section "Optional Murmur limits"
if ([string]::IsNullOrWhiteSpace($MurmurIni)) {
    Write-Status "murmur.ini" "Not checked (pass -MurmurIni <path> when the file is local)"
} elseif (!(Test-Path -LiteralPath $MurmurIni)) {
    Write-Status "murmur.ini" "NOT FOUND"
} else {
    $murmurText = Get-Content -LiteralPath $MurmurIni -Raw
    foreach ($setting in @(
        @{ Name = "pluginmessagelimit"; Expected = "20" },
        @{ Name = "pluginmessageburst"; Expected = "40" }
    )) {
        $match = [regex]::Match($murmurText, "(?mi)^\s*$($setting.Name)\s*=\s*([^\s#;]+)")
        if (!$match.Success) {
            Write-Status $setting.Name "MISSING"
        } elseif ($match.Groups[1].Value -eq $setting.Expected) {
            Write-Status $setting.Name ("OK (" + $setting.Expected + ")")
        } else {
            Write-Status $setting.Name ("MISMATCH (found " + $match.Groups[1].Value + ", expected " + $setting.Expected + ")")
        }
    }
}

Write-Output ""
Write-Output "Paste this report and the Arma RPT after a test if a state is unexpected."
