param(
    [ValidatePattern('^[A-Za-z0-9_.:-]{1,96}$')][string]$Mission,
    [string]$MumblePath = "$env:ProgramFiles\Mumble\client\mumble.exe"
)
$ErrorActionPreference = "Stop"
if (!(Test-Path -LiteralPath $MumblePath)) { throw "No se encontró Mumble: $MumblePath" }
if (Get-Process mumble -ErrorAction SilentlyContinue) { throw "Cerrá Mumble antes de iniciar una nueva misión." }
if ([string]::IsNullOrWhiteSpace($Mission)) {
    Remove-Item Env:MUMBLEACRE_MISSION -ErrorAction SilentlyContinue
} else {
    $env:MUMBLEACRE_MISSION = $Mission
}
Start-Process -FilePath $MumblePath
