# openharn launcher — starts a local MiniCPM llama-server (if not already up) and
# opens the interactive agent REPL. Usage:
#   .\openharn.ps1 [target-directory]          fast mode (thinking off, default)
#   .\openharn.ps1 [target-directory] -Think   hybrid thinking ON (slower, smarter)
param(
    [string]$Dir = ".",
    [switch]$Think
)

$Port        = 8080
$Model       = "C:\Users\Paper\Downloads\MiniCPM-V-4_6-Q8_0.gguf"
$LlamaServer = "C:\Users\Paper\AppData\Local\Microsoft\WinGet\Packages\ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe\llama-server.exe"
$Exe         = Join-Path $PSScriptRoot "target\debug\openharn.exe"

if (-not (Test-Path $Exe)) {
    Write-Host "building openharn..." -ForegroundColor Cyan
    Push-Location $PSScriptRoot; cargo build; Pop-Location
}

# MiniCPM 4.6 is a hybrid-thinking model; its template defaults thinking OFF.
# -Think flips it on via the chat template kwargs (llama-server then streams the
# reasoning separately, which openharn renders dimmed).
$thinkArgs = if ($Think) { ' --chat-template-kwargs "{\"enable_thinking\":true}" --reasoning-format deepseek' } else { '' }

# If a server is already up but in the wrong think-mode, restart it.
$marker = Join-Path $env:TEMP "openharn-think-mode.txt"
$wantMode = if ($Think) { "think" } else { "fast" }
$listening = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
$currentMode = if (Test-Path $marker) { Get-Content $marker -ErrorAction SilentlyContinue } else { "" }
if ($listening -and $currentMode -ne $wantMode) {
    Write-Host "restarting model in $wantMode mode..." -ForegroundColor Cyan
    $listening | ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
    $listening = $null
}

if (-not $listening) {
    Write-Host "starting MiniCPM (0.8B, $wantMode mode) on :$Port ..." -ForegroundColor Cyan
    Start-Process -FilePath $LlamaServer -WindowStyle Hidden -ArgumentList `
        ("-m `"$Model`" --jinja --ctx-size 16384 -ngl 99 --host 127.0.0.1 --port $Port --no-warmup" + $thinkArgs)
    Set-Content $marker $wantMode
    for ($i = 0; $i -lt 60; $i++) {
        try { if ((Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2).status -eq "ok") { break } } catch {}
        Start-Sleep -Milliseconds 800
    }
    Write-Host "model ready.`n" -ForegroundColor Green
} else {
    Write-Host "reusing model already running on :$Port ($wantMode mode)`n" -ForegroundColor DarkGray
}

$env:OPENHARN_BASE_URL = "http://127.0.0.1:$Port/v1"
$env:OPENHARN_MODEL    = "minicpm"
& $Exe $Dir
