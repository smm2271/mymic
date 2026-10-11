$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot/../..").Path
$artifact = Join-Path $root 'artifacts'
New-Item -ItemType Directory -Path $artifact -Force | Out-Null
Push-Location $root
try {
    gcc -std=c11 -Wall -Wextra -Werror -Wno-misleading-indentation -Itests/host/include -Itests/host/vendor -Ifirmware/esp32-tx/main tests/host/golden_test.c tests/host/lab_golden.c tests/host/crypto_adapter.c tests/host/vendor/cJSON.c firmware/esp32-tx/main/protocol.c firmware/esp32-tx/main/audio_source.c -lbcrypt -lm -o artifacts/c-golden-test.exe
    if ($LASTEXITCODE -ne 0) { throw 'C host build failed' }
    & ./artifacts/c-golden-test.exe tests/vectors.json tests/lab-vectors.json
    if ($LASTEXITCODE -ne 0) { throw 'C golden test failed' }
} finally { Pop-Location }
