$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot/..").Path
Push-Location $root
try {
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'Rust formatting failed' }
    cargo test --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    cargo build --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
    Push-Location web/angular
    try {
        npm.cmd ci
        if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
        npm.cmd run build
        if ($LASTEXITCODE -ne 0) { throw 'Angular build failed' }
        npm.cmd test
        if ($LASTEXITCODE -ne 0) { throw 'Frontend history tests failed' }
    } finally { Pop-Location }
    python -X utf8 tests/integration.py
    if ($LASTEXITCODE -ne 0) { throw 'Integration failed' }
    python -X utf8 tests/telemetry-integration.py
    if ($LASTEXITCODE -ne 0) { throw 'Telemetry integration failed' }
    python -X utf8 tests/diagnostics-integration.py
    if ($LASTEXITCODE -ne 0) { throw 'Continuity diagnostics integration failed' }
    & tests/host/run.ps1
    if ($LASTEXITCODE -ne 0) { throw 'C host test failed' }
} finally { Pop-Location }
