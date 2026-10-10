param(
    [string]$IdfToolsPath = 'C:/Espressif',
    [string]$IdfPath = 'C:/Espressif/frameworks/esp-idf-v5.5.5',
    [string]$IdfPython = 'C:/Espressif/python_env/idf5.5_py3.11_env/Scripts/python.exe'
)
$ErrorActionPreference = 'Stop'
$env:IDF_TOOLS_PATH = $IdfToolsPath
$env:IDF_PATH = $IdfPath
$env:Path = "$(Split-Path $IdfPython);$env:Path"
. "$IdfPath/export.ps1"
Push-Location "$PSScriptRoot/../firmware/esp32-tx"
try {
    idf.py build
    if ($LASTEXITCODE -ne 0) { throw 'ESP-IDF build failed' }
} finally { Pop-Location }
