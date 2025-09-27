# Ensures dependencies are ready with uv and starts the FastAPI backend.
$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $scriptDir

try {
    if (-not (Get-Command 'uv' -ErrorAction SilentlyContinue)) {
        Write-Error "uv was not found on PATH. Install uv from https://astral.sh/uv and rerun this script."
        exit 1
    }

    $venvDir = Join-Path $scriptDir '.venv'
    $lockFile = Join-Path $scriptDir 'uv.lock'
    $needsSetup = -not (Test-Path $venvDir) -or -not (Test-Path $lockFile)

    if ($needsSetup) {
        Write-Host '[uv] Initializing project environment...'
        uv sync
    } else {
        Write-Host '[uv] Existing environment detected. Skipping sync.'
    }

    Write-Host '[uv] Launching FastAPI service on http://localhost:8001'
    & uv run uvicorn main:app --host 0.0.0.0 --port 8001 --reload
    $exitCode = $LASTEXITCODE

    if ($exitCode -ne 0) {
        Write-Error "Service exited with code $exitCode"
        exit $exitCode
    }
}
finally {
    Pop-Location
}
