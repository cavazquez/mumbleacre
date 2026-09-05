param(
    [Parameter(Mandatory=$true)][ValidatePattern('^[A-Za-z0-9_.:-]{1,96}$')][string]$Mission,
    [string]$MumblePath = "$env:ProgramFiles\Mumble\mumble.exe"
)
$ErrorActionPreference = "Stop"
if (!(Test-Path -LiteralPath $MumblePath)) { throw "No se encontró Mumble: $MumblePath" }
if (Get-Process mumble -ErrorAction SilentlyContinue) { throw "Cerrá Mumble antes de iniciar una nueva misión." }
$env:MUMBLEACRE_MISSION = $Mission
Start-Process -FilePath $MumblePath
