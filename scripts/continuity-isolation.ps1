param(
    [ValidateSet('all-on','fade-off','capacity-old','recovery-old','all-old')]
    [string]$Profile = 'all-on',
    [Parameter(Mandatory=$true)][string]$Executable
)
$ErrorActionPreference = 'Stop'
$env:MIC_TEST_OUTPUT_FADE = 'on'
$env:MIC_TEST_CAPACITY_POLICY = 'on'
$env:MIC_TEST_RECOVERY_POLICY = 'on'
switch ($Profile) {
    'fade-off' { $env:MIC_TEST_OUTPUT_FADE = 'off' }
    'capacity-old' { $env:MIC_TEST_CAPACITY_POLICY = 'off' }
    'recovery-old' { $env:MIC_TEST_RECOVERY_POLICY = 'off' }
    'all-old' { $env:MIC_TEST_OUTPUT_FADE = 'off'; $env:MIC_TEST_CAPACITY_POLICY = 'off'; $env:MIC_TEST_RECOVERY_POLICY = 'off' }
}
$resolved = (Resolve-Path -LiteralPath $Executable).Path
Write-Host "Profile: $Profile"
& $resolved --version
Get-FileHash -LiteralPath $resolved -Algorithm SHA256 | Format-List
& $resolved
exit $LASTEXITCODE
