# Offscreen signed-sample plots using the standard .NET Charting library.
# Run the Rust export_continuity_comparisons test first; no audio devices are used.
param([string]$Directory = "$PSScriptRoot/../artifacts/v063-phase1")
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms.DataVisualization
Add-Type -AssemblyName System.Drawing
$plotDirectory = (Resolve-Path -LiteralPath $Directory).Path
$records = Get-Content -LiteralPath "$plotDirectory/comparison.json" -Raw | ConvertFrom-Json
foreach ($record in $records) {
    $chart = New-Object System.Windows.Forms.DataVisualization.Charting.Chart
    try {
        $chart.Size = New-Object System.Drawing.Size(1400, 760)
        $chart.BackColor = [System.Drawing.Color]::White
        $title = $chart.Titles.Add("$($record.case) - final output samples, before / after")
        $title.Font = New-Object System.Drawing.Font('Arial', 16)
        $seam = [int]$record.seam
        $rate = [int]$record.rate
        foreach ($version in @('before', 'after')) {
            $area = New-Object System.Windows.Forms.DataVisualization.Charting.ChartArea($version)
            $area.AxisX.Title = 'Time relative to seam (ms)'
            $area.AxisX.Minimum = -4
            $area.AxisX.Maximum = 6
            $area.AxisX.Interval = 1
            $area.AxisY.Title = 'PCM normalized (full scale +/-1)'
            $area.AxisY.Minimum = -1
            $area.AxisY.Maximum = 1
            $area.AxisY.Interval = 0.25
            $area.AxisX.MajorGrid.LineColor = [System.Drawing.Color]::LightGray
            $area.AxisY.MajorGrid.LineColor = [System.Drawing.Color]::LightGray
            $line = New-Object System.Windows.Forms.DataVisualization.Charting.StripLine
            $line.IntervalOffset = 0
            $line.BorderColor = [System.Drawing.Color]::Gray
            $line.BorderWidth = 2
            $line.BorderDashStyle = 'Dash'
            $area.AxisX.StripLines.Add($line)
            $chart.ChartAreas.Add($area)
            $series = New-Object System.Windows.Forms.DataVisualization.Charting.Series($version)
            $series.ChartArea = $version
            $series.ChartType = 'Line'
            $series.BorderWidth = 2
            $series.Color = if ($version -eq 'before') { [System.Drawing.Color]::DarkOrange } else { [System.Drawing.Color]::Teal }
            $series.LegendText = $version
            $bytes = [System.IO.File]::ReadAllBytes("$plotDirectory/$($record.case)-$version.wav")
            if ([BitConverter]::ToInt32($bytes, 24) -ne $rate -or [BitConverter]::ToInt16($bytes, 22) -ne 1 -or [BitConverter]::ToInt16($bytes, 34) -ne 16) { throw 'Unexpected exported WAV format' }
            $count = ($bytes.Length - 44) / 2
            $first = [Math]::Max(0, [int][Math]::Floor($seam - 0.004 * $rate))
            $last = [Math]::Min($count - 1, [int][Math]::Ceiling($seam + 0.006 * $rate))
            for ($index = $first; $index -le $last; $index++) {
                $sample = [BitConverter]::ToInt16($bytes, 44 + $index * 2) / 32768.0
                $null = $series.Points.AddXY(($index - $seam) * 1000.0 / $rate, $sample)
            }
            $chart.Series.Add($series)
            $legend = New-Object System.Windows.Forms.DataVisualization.Charting.Legend($version)
            $legend.DockedToChartArea = $version
            $legend.IsDockedInsideChartArea = $true
            $chart.Legends.Add($legend)
            $series.Legend = $version
        }
        $path = "$plotDirectory/$($record.case)-comparison.png"
        $chart.SaveImage($path, [System.Windows.Forms.DataVisualization.Charting.ChartImageFormat]::Png)
        Write-Output "WROTE $path"
    } finally { $chart.Dispose() }
}
